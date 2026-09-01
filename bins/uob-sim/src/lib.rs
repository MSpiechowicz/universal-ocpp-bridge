use std::collections::VecDeque;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ocpp_client::ocpp_types::v16::common::ResetResponseStatus;
use ocpp_client::ocpp_types::v16::{
    HeartbeatRequest as HeartbeatRequest16, ResetResponse as ResetResponse16,
};
use ocpp_client::ocpp_types::v201::common::ResetStatusEnum;
use ocpp_client::ocpp_types::v201::{
    HeartbeatRequest as HeartbeatRequest201, ResetResponse as ResetResponse201,
};
use ocpp_client::{
    ConnectOptions, KeepaliveBehavior, ReconnectBehavior, ReconnectPolicy, connect_1_6,
    connect_2_0_1,
};
use tokio::sync::{mpsc, oneshot};

pub const PINNED_OCPP_CLIENT_VERSION: &str = "0.4.0";

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
}

pub type ClientFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SimulatorClientError>> + Send + 'a>>;

pub trait ProtocolClient: Send + Sync {
    fn version(&self) -> OcppVersion;
    fn heartbeat(&self) -> ClientFuture<'_, String>;
    fn shutdown(&self) -> ClientFuture<'_, ()>;
    fn traces(&self) -> Vec<TraceEvent>;
}

#[derive(Clone)]
pub struct SimulatorProtocolClient {
    version: OcppVersion,
    commands: mpsc::Sender<Command>,
    traces: TraceBuffer,
}

enum Command {
    Heartbeat(oneshot::Sender<Result<String, SimulatorClientError>>),
    Shutdown(oneshot::Sender<Result<(), SimulatorClientError>>),
}

#[derive(Clone)]
struct TraceBuffer {
    capacity: usize,
    events: Arc<Mutex<VecDeque<TraceEvent>>>,
}

impl TraceBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            events: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
        }
    }

    fn push(&self, kind: TraceKind, detail: impl Into<String>) {
        let mut events = self.events.lock().expect("trace buffer lock poisoned");
        if events.len() == self.capacity {
            events.pop_front();
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
}

impl SimulatorProtocolClient {
    /// Connects the simulator-owned client and starts its bounded command worker.
    ///
    /// # Errors
    ///
    /// Returns an error when either capacity is zero or the initial WebSocket connection and
    /// OCPP subprotocol negotiation fails.
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

        match config.version {
            OcppVersion::V1_6 => {
                let client = connect_1_6(&config.endpoint, Some(options))
                    .await
                    .map_err(|error| SimulatorClientError::Connection(error.to_string()))?;
                register_1_6_handlers(&client, &traces).await;
                register_1_6_reconnect(&client, &traces).await;
                traces.push(TraceKind::Connected, OcppVersion::V1_6.websocket_protocol());
                tokio::spawn(run_1_6(client, receiver, traces.clone()));
            }
            OcppVersion::V2_0_1 => {
                let client = connect_2_0_1(&config.endpoint, Some(options))
                    .await
                    .map_err(|error| SimulatorClientError::Connection(error.to_string()))?;
                register_2_0_1_handlers(&client, &traces).await;
                register_2_0_1_reconnect(&client, &traces).await;
                traces.push(
                    TraceKind::Connected,
                    OcppVersion::V2_0_1.websocket_protocol(),
                );
                tokio::spawn(run_2_0_1(client, receiver, traces.clone()));
            }
        }

        Ok(Self {
            version: config.version,
            commands,
            traces,
        })
    }

    async fn send_command<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, SimulatorClientError>>) -> Command,
    ) -> Result<T, SimulatorClientError> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .try_send(build(sender))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SimulatorClientError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => SimulatorClientError::Stopped,
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

    fn shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move { self.send_command(Command::Shutdown).await })
    }

    fn traces(&self) -> Vec<TraceEvent> {
        self.traces.snapshot()
    }
}

async fn register_1_6_handlers(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reset(move |_request, _client| {
            traces.push(TraceKind::ResetReceived, "accepted");
            async move {
                Ok(ResetResponse16 {
                    status: ResetResponseStatus::Accepted,
                })
            }
        })
        .await;
}

async fn register_2_0_1_handlers(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reset(move |_request, _client| {
            traces.push(TraceKind::ResetReceived, "accepted");
            async move {
                Ok(ResetResponse201 {
                    custom_data: None,
                    status: ResetStatusEnum::Accepted,
                    status_info: None,
                })
            }
        })
        .await;
}

async fn register_1_6_reconnect(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reconnect(move |_| {
            traces.push(TraceKind::Reconnected, "ocpp1.6");
            async {}
        })
        .await;
}

async fn register_2_0_1_reconnect(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    traces: &TraceBuffer,
) {
    let traces = traces.clone();
    client
        .on_reconnect(move |_| {
            traces.push(TraceKind::Reconnected, "ocpp2.0.1");
            async {}
        })
        .await;
}

async fn run_1_6(
    client: ocpp_client::ocpp_1_6::OCPP1_6Client,
    mut commands: mpsc::Receiver<Command>,
    traces: TraceBuffer,
) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Heartbeat(result) => {
                traces.push(TraceKind::HeartbeatSent, "Heartbeat");
                let response = client
                    .send_heartbeat(HeartbeatRequest16 {})
                    .await
                    .map(|response| response.current_time.to_string())
                    .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                record_result(&traces, &response);
                let _ = result.send(response);
            }
            Command::Shutdown(result) => {
                let response = client
                    .disconnect()
                    .await
                    .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                traces.push(TraceKind::Stopped, "client disconnected");
                let _ = result.send(response);
                break;
            }
        }
    }
}

async fn run_2_0_1(
    client: ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    mut commands: mpsc::Receiver<Command>,
    traces: TraceBuffer,
) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Heartbeat(result) => {
                traces.push(TraceKind::HeartbeatSent, "Heartbeat");
                let response = client
                    .send_heartbeat(HeartbeatRequest201 { custom_data: None })
                    .await
                    .map(|response| response.current_time.to_string())
                    .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                record_result(&traces, &response);
                let _ = result.send(response);
            }
            Command::Shutdown(result) => {
                let response = client
                    .disconnect()
                    .await
                    .map_err(|error| SimulatorClientError::Protocol(error.to_string()));
                traces.push(TraceKind::Stopped, "client disconnected");
                let _ = result.send(response);
                break;
            }
        }
    }
}

fn record_result(traces: &TraceBuffer, response: &Result<String, SimulatorClientError>) {
    match response {
        Ok(timestamp) => traces.push(TraceKind::HeartbeatResult, timestamp),
        Err(error) => traces.push(TraceKind::Failed, error.to_string()),
    }
}
