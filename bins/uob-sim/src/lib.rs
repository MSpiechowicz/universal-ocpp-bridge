use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ocpp_client::{
    ConnectOptions, KeepaliveBehavior, ReconnectBehavior, ReconnectPolicy, connect_1_6,
    connect_2_0_1,
};
use tokio::sync::{mpsc, oneshot};

mod client_runtime;
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
    pub version: OcppVersion,
    pub request_timeout: Duration,
    pub reconnect: bool,
    pub command_capacity: usize,
    pub trace_capacity: usize,
    pub connectors: Vec<u16>,
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

pub type ClientFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SimulatorClientError>> + Send + 'a>>;

pub trait ProtocolClient: Send + Sync {
    fn version(&self) -> OcppVersion;
    fn heartbeat(&self) -> ClientFuture<'_, String>;
    fn call(&self, _call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        Box::pin(async {
            Err(SimulatorClientError::Protocol(
                "charging calls are unsupported by this client".to_owned(),
            ))
        })
    }
    fn next_remote_command(&self) -> ClientFuture<'_, RemoteCommand> {
        Box::pin(async {
            Err(SimulatorClientError::Protocol(
                "remote commands are unsupported by this client".to_owned(),
            ))
        })
    }
    fn shutdown(&self) -> ClientFuture<'_, ()>;
    fn force_shutdown(&self) -> ClientFuture<'_, ()>;
    fn abort(&self);
    fn traces(&self) -> Vec<TraceEvent>;
    fn diagnostics(&self) -> ClientDiagnostics {
        ClientDiagnostics::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClientDiagnostics {
    pub rejected_commands: u64,
    pub dropped_traces: u64,
}

#[derive(Clone)]
pub struct SimulatorProtocolClient {
    version: OcppVersion,
    commands: mpsc::Sender<Command>,
    traces: TraceBuffer,
    worker: tokio::task::AbortHandle,
    emergency_client: EmergencyClient,
    rejected_commands: Arc<AtomicU64>,
}

enum Command {
    Heartbeat(oneshot::Sender<Result<String, SimulatorClientError>>),
    Call(
        SimulatorCall,
        oneshot::Sender<Result<serde_json::Value, SimulatorClientError>>,
    ),
    NextRemote(oneshot::Sender<Result<RemoteCommand, SimulatorClientError>>),
    Shutdown(oneshot::Sender<Result<(), SimulatorClientError>>),
}

#[derive(Clone)]
enum EmergencyClient {
    V1_6(ocpp_client::ocpp_1_6::OCPP1_6Client),
    V2_0_1(ocpp_client::ocpp_2_0_1::OCPP2_0_1Client),
}

impl EmergencyClient {
    async fn disconnect(&self) -> Result<(), SimulatorClientError> {
        match self {
            Self::V1_6(client) => client
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
    pub async fn connect(config: SimulatorClientConfig) -> Result<Self, SimulatorClientError> {
        if config.command_capacity == 0 {
            return Err(SimulatorClientError::InvalidCapacity("command_capacity"));
        }
        if config.trace_capacity == 0 {
            return Err(SimulatorClientError::InvalidCapacity("trace_capacity"));
        }

        let traces = TraceBuffer::new(config.trace_capacity);
        let options = ConnectOptions {
            timeout: Some(config.request_timeout),
            reconnect: if config.reconnect {
                ReconnectBehavior::Enabled(ReconnectPolicy::default())
            } else {
                ReconnectBehavior::Disabled
            },
            keepalive: KeepaliveBehavior::Disabled,
            ..ConnectOptions::default()
        };
        let (commands, receiver) = mpsc::channel(config.command_capacity);
        let (remote_commands, remote_receiver) = mpsc::channel(config.command_capacity);
        let rejected_commands = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Ocpp16State::default()));

        let (worker, emergency_client) = match config.version {
            OcppVersion::V1_6 => {
                let client = connect_1_6(&config.endpoint, Some(options))
                    .await
                    .map_err(|error| SimulatorClientError::Connection(error.to_string()))?;
                client_runtime::register_1_6_handlers(
                    &client,
                    &traces,
                    remote_commands,
                    config.connectors.clone(),
                    Arc::clone(&state),
                )
                .await;
                client_runtime::register_1_6_reconnect(&client, &traces).await;
                traces.push(TraceKind::Connected, OcppVersion::V1_6.websocket_protocol());
                let emergency_client = EmergencyClient::V1_6(client.clone());
                let worker = tokio::spawn(client_runtime::run_1_6(
                    client,
                    receiver,
                    traces.clone(),
                    config.command_capacity,
                    remote_receiver,
                    state,
                ))
                .abort_handle();
                (worker, emergency_client)
            }
            OcppVersion::V2_0_1 => {
                let client = connect_2_0_1(&config.endpoint, Some(options))
                    .await
                    .map_err(|error| SimulatorClientError::Connection(error.to_string()))?;
                client_runtime::register_2_0_1_handlers(&client, &traces).await;
                client_runtime::register_2_0_1_reconnect(&client, &traces).await;
                traces.push(
                    TraceKind::Connected,
                    OcppVersion::V2_0_1.websocket_protocol(),
                );
                let emergency_client = EmergencyClient::V2_0_1(client.clone());
                let worker = tokio::spawn(client_runtime::run_2_0_1(
                    client,
                    receiver,
                    traces.clone(),
                    config.command_capacity,
                ))
                .abort_handle();
                (worker, emergency_client)
            }
        };

        Ok(Self {
            version: config.version,
            commands,
            traces,
            worker,
            emergency_client,
            rejected_commands,
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

impl ProtocolClient for SimulatorProtocolClient {
    fn version(&self) -> OcppVersion {
        self.version
    }

    fn heartbeat(&self) -> ClientFuture<'_, String> {
        Box::pin(async move { self.send_command(Command::Heartbeat).await })
    }

    fn call(&self, call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        Box::pin(async move {
            self.send_command(|result| Command::Call(call, result))
                .await
        })
    }

    fn next_remote_command(&self) -> ClientFuture<'_, RemoteCommand> {
        Box::pin(async move { self.send_command(Command::NextRemote).await })
    }

    fn shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move { self.send_command(Command::Shutdown).await })
    }

    fn force_shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move { self.emergency_client.disconnect().await })
    }

    fn abort(&self) {
        self.traces
            .push(TraceKind::Stopped, "client task force-stopped");
        self.worker.abort();
    }

    fn traces(&self) -> Vec<TraceEvent> {
        self.traces.snapshot()
    }

    fn diagnostics(&self) -> ClientDiagnostics {
        ClientDiagnostics {
            rejected_commands: self.rejected_commands.load(Ordering::Relaxed),
            dropped_traces: self.traces.dropped(),
        }
    }
}
