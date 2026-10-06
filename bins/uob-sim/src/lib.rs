pub mod artifact_transfer;
pub mod control;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

mod client_api;
mod client_exchange16;
mod client_impl;
pub use client_api::{ClientDiagnostics, ClientFuture, ProtocolClient};
mod client_lifecycle16;
mod client_observation16;
mod client_reconnect201;
mod client_runtime;
mod client_runtime_201;
pub mod local_authorization;
pub mod local_authorization201;
mod native_state;
pub mod reservation16;
pub mod reservation201;
use native_state::{open_native_state, open_native_state201, open_reservation201};
mod station_auth;
mod trigger;
mod trigger201;
mod trigger_transport;
pub use trigger::{TriggerObservation, TriggerReply, TriggerResponses};
pub mod scenario;

pub const PINNED_OCPP_CLIENT_VERSION: &str = "0.5.0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OcppVersion {
    V1_6,
    V2_0_1,
}

impl OcppVersion {
    #[must_use]
    pub const fn websocket_protocol(self) -> &'static str {
        match self {
            Self::V1_6 => "ocpp1.6",
            Self::V2_0_1 => "ocpp2.0.1",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceKind {
    Connected,
    HeartbeatSent,
    HeartbeatResult,
    ResetReceived,
    RemoteStartReceived,
    RemoteStopReceived,
    ChargingCallSent,
    TriggerReceived,
    ChargingCallResult,
    Reconnected,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceEvent {
    pub kind: TraceKind,
    pub detail: String,
}

#[derive(Debug)]
pub enum SimulatorClientError {
    InvalidCapacity(&'static str),
    Connection(String),
    QueueFull,
    Stopped,
    Protocol(String),
}

impl Display for SimulatorClientError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCapacity(name) => write!(formatter, "{name} must be greater than zero"),
            Self::Connection(message) => write!(formatter, "connection failed: {message}"),
            Self::QueueFull => formatter.write_str("simulator client command queue is full"),
            Self::Stopped => formatter.write_str("simulator client is stopped"),
            Self::Protocol(message) => write!(formatter, "OCPP exchange failed: {message}"),
        }
    }
}

impl Error for SimulatorClientError {}

#[derive(Clone, Debug)]
pub struct SimulatorClientConfig {
    pub endpoint: String,
    pub credentials_file: Option<String>,
    pub version: OcppVersion,
    pub request_timeout: Duration,
    pub reconnect: bool,
    pub command_capacity: usize,
    pub trace_capacity: usize,
    pub connectors: Vec<u16>,
    pub evse_connectors: Vec<(u16, u16)>,
    pub trigger_responses: TriggerResponses,
    pub trigger_observation: TriggerObservation,
    pub local_authorization: Option<local_authorization::LocalAuthorizationHandle>,
    pub local_authorization_file: Option<(String, local_authorization::LocalAuthorizationConfig)>,
    pub reservation16: Option<(String, reservation16::ReservationConfig)>,
    pub reservation201: Option<(String, reservation201::Reservation201Config)>,
}

/// A simulator-owned OCPP call that retains exact native JSON field values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulatorCall {
    pub action: SimulatorAction,
    pub payload: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulatorAction {
    BootNotification,
    Authorize,
    StatusNotification,
    StartTransaction,
    MeterValues,
    StopTransaction,
}

impl SimulatorAction {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::BootNotification => "BootNotification",
            Self::Authorize => "Authorize",
            Self::StatusNotification => "StatusNotification",
            Self::StartTransaction => "StartTransaction",
            Self::MeterValues => "MeterValues",
            Self::StopTransaction => "StopTransaction",
        }
    }

    #[must_use]
    pub const fn wire_name(self, version: OcppVersion) -> &'static str {
        match (version, self) {
            (
                OcppVersion::V2_0_1,
                Self::StartTransaction | Self::MeterValues | Self::StopTransaction,
            ) => "TransactionEvent",
            _ => self.name(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteCommandKind {
    StartTransaction,
    StopTransaction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteCommand {
    pub kind: RemoteCommandKind,
    pub payload: serde_json::Value,
    pub accepted: bool,
}

/// The receipt fires only after the socket handler has held the remote reply for the delay.
pub type ReplyDelayReceipt = oneshot::Receiver<()>;

struct ReplyDelay {
    kind: RemoteCommandKind,
    duration: Duration,
    receipt: oneshot::Sender<()>,
}

type ReplyDelaySlot = Arc<Mutex<Option<ReplyDelay>>>;

fn take_reply_delay(slot: &ReplyDelaySlot, kind: RemoteCommandKind) -> Option<ReplyDelay> {
    let mut slot = slot.lock().expect("reply delay lock poisoned");
    if slot.as_ref().is_some_and(|delay| delay.kind == kind) {
        slot.take()
    } else {
        None
    }
}

#[derive(Clone)]
pub struct SimulatorProtocolClient {
    version: OcppVersion,
    commands: mpsc::Sender<Command>,
    traces: TraceBuffer,
    worker: tokio::task::AbortHandle,
    emergency_client: EmergencyClient,
    rejected_commands: Arc<AtomicU64>,
    reply_delay: ReplyDelaySlot,
    ocpp16_state: Option<Arc<Mutex<Ocpp16State>>>,
    ocpp201_state: Option<Arc<Mutex<Ocpp201State>>>,
}

enum Command {
    Heartbeat(oneshot::Sender<Result<String, SimulatorClientError>>),
    Call(
        SimulatorCall,
        oneshot::Sender<Result<serde_json::Value, SimulatorClientError>>,
    ),
    NextRemote(oneshot::Sender<Result<RemoteCommand, SimulatorClientError>>),
    LocalListConflict,
    ReservationStatus(u32, &'static str),
    Shutdown(oneshot::Sender<Result<(), SimulatorClientError>>),
}

#[derive(Clone)]
enum EmergencyClient {
    V1_6(Arc<tokio::sync::Mutex<ocpp_client::ocpp_1_6::OCPP1_6Client>>),
    V2_0_1(ocpp_client::ocpp_2_0_1::OCPP2_0_1Client),
}

impl EmergencyClient {
    async fn disconnect(&self) -> Result<(), SimulatorClientError> {
        match self {
            Self::V1_6(client) => client
                .lock()
                .await
                .disconnect()
                .await
                .map_err(|error| SimulatorClientError::Protocol(error.to_string())),
            Self::V2_0_1(client) => client
                .disconnect()
                .await
                .map_err(|error| SimulatorClientError::Protocol(error.to_string())),
        }
    }
}

#[derive(Clone)]
struct TraceBuffer {
    capacity: usize,
    events: Arc<Mutex<VecDeque<TraceEvent>>>,
    dropped: Arc<AtomicU64>,
}

#[derive(Default)]
struct Ocpp16State {
    active_transactions: HashMap<i64, u16>,
    registered: bool,
    boot: Option<serde_json::Value>,
    status: HashMap<u16, serde_json::Value>,
    meters: HashMap<u16, serde_json::Value>,
    local: Option<local_authorization::LocalAuthorizationHandle>,
    reservation16: Option<reservation16::ReservationHandle>,
    reservation_notifications: Vec<(u32, &'static str)>,
    local_reply_fault: Option<local_authorization::transport::NativeReplyFault>,
    reset_reason: Option<ocpp_client::ocpp_types::v16::common::Reason>,
    reboot_count: u64,
    socket_connected: bool,
    socket_generation: u64,
    // Trigger eligibility is scoped to the socket that accepted the Boot,
    // independently of ordinary reconnect's retained charging registration.
    boot_accepted_generation: Option<u64>,
    notifications: Option<mpsc::WeakSender<Command>>,
    replay_requested: bool,
}

#[derive(Default)]
struct Ocpp201State {
    active_transactions: HashMap<String, (u16, u16)>,
    transactions: HashMap<String, serde_json::Value>,
    status: HashMap<(u16, u16), serde_json::Value>,
    meters: HashMap<u16, serde_json::Value>,
    boot: Option<serde_json::Value>,
    registered: bool,
    local: Option<local_authorization201::LocalAuthorization201Handle>,
    local_reply_fault: Option<local_authorization::transport::NativeReplyFault>,
    reservation201: Option<reservation201::Reservation201Handle>,
    reservation_retry_at: Option<std::time::Instant>,
    reservation_holds: usize,
    socket_connected: bool,
    socket_generation: u64,
    reboot_count: u64,
}

impl TraceBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            events: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    fn push(&self, kind: TraceKind, detail: impl Into<String>) {
        let mut events = self.events.lock().expect("trace buffer lock poisoned");
        if events.len() == self.capacity {
            events.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        events.push_back(TraceEvent {
            kind,
            detail: detail.into(),
        });
    }

    fn snapshot(&self) -> Vec<TraceEvent> {
        self.events
            .lock()
            .expect("trace buffer lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl SimulatorProtocolClient {
    /// Connects the simulator-owned client and starts its bounded command worker.
    ///
    /// # Errors
    ///
    /// Returns an error when capacities are zero or WebSocket/OCPP negotiation fails.
    #[allow(clippy::too_many_lines)] // Session negotiation and every native handler are wired in one place.
    pub async fn connect(config: SimulatorClientConfig) -> Result<Self, SimulatorClientError> {
        validate_client_config(&config)?;

        let traces = TraceBuffer::new(config.trace_capacity);
        let (commands, receiver) = mpsc::channel(config.command_capacity);
        let (remote_commands, remote_receiver) = mpsc::channel(config.command_capacity);
        let reply_delay = Arc::new(Mutex::new(None));
        let rejected_commands = Arc::new(AtomicU64::new(0));
        let (worker, emergency_client, ocpp16_state, ocpp201_state) = match config.version {
            OcppVersion::V1_6 => {
                let local = open_native_state(&config)?;
                let state = Arc::new(Mutex::new(Ocpp16State {
                    local: Some(local),
                    reservation16: config
                        .reservation16
                        .as_ref()
                        .map(|(station, options)| {
                            reservation16::ReservationHandle::open(
                                station,
                                &config.connectors,
                                options,
                            )
                            .map_err(|code| SimulatorClientError::Protocol(code.to_owned()))
                        })
                        .transpose()?,
                    notifications: Some(commands.downgrade()),
                    ..Ocpp16State::default()
                }));
                let (worker, emergency_client) = client_runtime::connect_and_run_1_6(
                    &config,
                    &traces,
                    remote_commands,
                    receiver,
                    remote_receiver,
                    Arc::clone(&state),
                    reply_delay.clone(),
                )
                .await?;
                (worker, emergency_client, Some(state), None)
            }
            OcppVersion::V2_0_1 => {
                let local = open_native_state201(&config)?;
                let state = Arc::new(Mutex::new(Ocpp201State {
                    local: Some(local),
                    reservation201: open_reservation201(&config)?,
                    ..Ocpp201State::default()
                }));
                let (client, barrier, jobs) = trigger_transport::connect_201(
                    &config.endpoint,
                    config.credentials_file.as_deref(),
                    config.request_timeout,
                    config.reconnect,
                    config.command_capacity,
                    Arc::clone(&state),
                )
                .await
                .map_err(|error| SimulatorClientError::Connection(error.to_string()))?;
                trigger201::register(
                    &client,
                    barrier,
                    trigger201::TriggerContext201 {
                        resources: config.evse_connectors.clone(),
                        responses: config.trigger_responses.clone(),
                        observation: config.trigger_observation.clone(),
                        state: Arc::clone(&state),
                        traces: traces.clone(),
                    },
                    jobs,
                )
                .await;
                client_runtime_201::register_handlers(
                    &client,
                    &traces,
                    remote_commands,
                    config.evse_connectors.clone(),
                    Arc::clone(&state),
                    Arc::clone(&reply_delay),
                )
                .await;
                client_reconnect201::register(&client, Arc::clone(&state), traces.clone()).await;
                traces.push(
                    TraceKind::Connected,
                    OcppVersion::V2_0_1.websocket_protocol(),
                );
                let emergency_client = EmergencyClient::V2_0_1(client.clone());
                let worker = tokio::spawn(client_runtime_201::run(
                    client,
                    receiver,
                    traces.clone(),
                    config.command_capacity,
                    remote_receiver,
                    Arc::clone(&state),
                ))
                .abort_handle();
                (worker, emergency_client, None, Some(state))
            }
        };

        Ok(Self {
            version: config.version,
            commands,
            traces,
            worker,
            emergency_client,
            rejected_commands,
            reply_delay,
            ocpp16_state,
            ocpp201_state,
        })
    }

    async fn send_command<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, SimulatorClientError>>) -> Command,
    ) -> Result<T, SimulatorClientError> {
        let (sender, receiver) = oneshot::channel();
        self.commands.try_send(build(sender)).map_err(|error| {
            self.rejected_commands.fetch_add(1, Ordering::Relaxed);
            match error {
                mpsc::error::TrySendError::Full(_) => SimulatorClientError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => SimulatorClientError::Stopped,
            }
        })?;
        receiver.await.map_err(|_| SimulatorClientError::Stopped)?
    }
}

fn validate_client_config(config: &SimulatorClientConfig) -> Result<(), SimulatorClientError> {
    if config.command_capacity == 0 {
        return Err(SimulatorClientError::InvalidCapacity("command_capacity"));
    }
    if config.trace_capacity == 0 {
        return Err(SimulatorClientError::InvalidCapacity("trace_capacity"));
    }
    if config.version != OcppVersion::V1_6 && config.local_authorization.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 1.6 handle cannot attach to OCPP 2.0.1".to_owned(),
        ));
    }
    if config.version != OcppVersion::V1_6 && config.reservation16.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 1.6 reservations cannot attach to OCPP 2.0.1".to_owned(),
        ));
    }
    if config.version != OcppVersion::V2_0_1 && config.reservation201.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 2.0.1 reservations cannot attach to OCPP 1.6".to_owned(),
        ));
    }
    Ok(())
}
