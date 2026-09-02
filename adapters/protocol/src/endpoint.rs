use std::{
    collections::BTreeSet,
    error::Error,
    fmt, io,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use axum::{
    Router,
    extract::ws::{WebSocket, WebSocketUpgrade},
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::{net::TcpListener, sync::mpsc};
use uob_application::{Application, RuntimeResourceBudget, StationAdmission};
use uob_contracts::{Environment, StationId};

use crate::{
    AuthenticatedStation, StationAuthenticator, StationTlsAcceptor,
    StationTransportAdmissionRequest,
};

mod listener;

use listener::{PeerIdentity, TlsListener};

const OCPP_ROUTE: &str = "/ocpp/{station_id}";
const SUPPORTED_PROTOCOLS: [&str; 2] = ["ocpp1.6", "ocpp2.0.1"];
const GENERIC_AUTHENTICATION_FAILURE: &str = "station transport admission denied";

/// Axum OCPP endpoint with immutable authentication and shared resource policy.
#[derive(Clone)]
pub struct OcppEndpoint {
    state: EndpointState,
}

#[derive(Clone)]
struct EndpointState {
    authenticator: Arc<StationAuthenticator>,
    budget: RuntimeResourceBudget,
    active: ActiveStations,
    connections: mpsc::Sender<StationConnection>,
    maximum_message_bytes: usize,
    environment: Environment,
}

/// An authenticated socket and its connection-wide admission guards.
pub struct StationConnection {
    station: AuthenticatedStation,
    socket: WebSocket,
    _admission: ActiveStationAdmission,
}

impl StationConnection {
    /// Returns the configured station identity and negotiated OCPP edition.
    #[must_use]
    pub const fn station(&self) -> &AuthenticatedStation {
        &self.station
    }

    /// Receives the next bounded WebSocket message.
    pub async fn receive(&mut self) -> Option<Result<axum::extract::ws::Message, axum::Error>> {
        self.socket.recv().await
    }

    /// Sends one WebSocket message to the authenticated station.
    ///
    /// # Errors
    ///
    /// Returns an Axum socket error when the peer is unavailable or the frame cannot be written.
    pub async fn send(&mut self, message: axum::extract::ws::Message) -> Result<(), axum::Error> {
        self.socket.send(message).await
    }

    /// Requests a graceful WebSocket close.
    ///
    /// # Errors
    ///
    /// Returns an Axum socket error when the close frame cannot be written.
    pub async fn close(mut self) -> Result<(), axum::Error> {
        self.socket
            .send(axum::extract::ws::Message::Close(None))
            .await
    }
}

/// Consumer for authenticated sockets accepted by the endpoint.
pub struct StationConnectionReceiver {
    receiver: mpsc::Receiver<StationConnection>,
}

impl StationConnectionReceiver {
    /// Waits for the next authenticated station connection.
    pub async fn receive(&mut self) -> Option<StationConnection> {
        self.receiver.recv().await
    }

    /// Returns the configured number of accepted sockets that can await processing.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.receiver.max_capacity()
    }

    /// Returns the number of accepted sockets currently awaiting processing.
    #[must_use]
    pub fn backlog(&self) -> usize {
        self.receiver.len()
    }
}

impl OcppEndpoint {
    /// Creates a bounded endpoint and its single authenticated-connection receiver.
    ///
    /// # Errors
    ///
    /// Rejects a zero-capacity handoff queue before a listener is started.
    pub fn new(
        authenticator: StationAuthenticator,
        application: &Application,
        accepted_connection_capacity: usize,
    ) -> Result<(Self, StationConnectionReceiver), StationEndpointConfigurationError> {
        if accepted_connection_capacity == 0 {
            return Err(StationEndpointConfigurationError::ConnectionCapacity);
        }
        let budget = application.health().resources().clone();
        let maximum_message_bytes = budget.limits().maximum_ocpp_message_bytes;
        let (connections, receiver) = mpsc::channel(accepted_connection_capacity);
        Ok((
            Self {
                state: EndpointState {
                    authenticator: Arc::new(authenticator),
                    budget,
                    active: ActiveStations::default(),
                    connections,
                    maximum_message_bytes,
                    environment: application.runtime_identity().environment,
                },
            },
            StationConnectionReceiver { receiver },
        ))
    }

    /// Serves production WSS connections on an already bound address.
    ///
    /// # Errors
    ///
    /// Returns when the Axum server terminates with an I/O failure.
    pub async fn serve_tls(self, listener: TcpListener, tls: StationTlsAcceptor) -> io::Result<()> {
        let listener = TlsListener::new(listener, tls);
        axum::serve(
            listener,
            self.router()
                .into_make_service_with_connect_info::<PeerIdentity>(),
        )
        .await
    }

    /// Serves plaintext socket upgrades only in the explicitly selected demo environment.
    ///
    /// # Errors
    ///
    /// Rejects production or staging use before accepting a connection, and returns Axum I/O
    /// failures after a valid listener starts.
    pub async fn serve_plaintext(
        self,
        listener: TcpListener,
    ) -> Result<(), StationEndpointServeError> {
        if self.state.environment != Environment::Demo {
            return Err(StationEndpointServeError::PlaintextEnvironment);
        }
        axum::serve(
            listener,
            self.router()
                .into_make_service_with_connect_info::<PeerIdentity>(),
        )
        .await
        .map_err(StationEndpointServeError::Io)
    }

    fn router(self) -> Router {
        Router::new()
            .route(OCPP_ROUTE, any(upgrade_station))
            .with_state(self.state)
    }
}

async fn upgrade_station(
    State(state): State<EndpointState>,
    ConnectInfo(peer): ConnectInfo<PeerIdentity>,
    Path(raw_station_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(protocol) = select_protocol(&ws) else {
        return rejection(StatusCode::BAD_REQUEST, "unsupported OCPP subprotocol");
    };
    let Ok(station_id) = StationId::new(raw_station_id) else {
        return authentication_rejection();
    };
    let Some(credential) = basic_credential(&headers, &station_id) else {
        return authentication_rejection();
    };
    let Ok(authenticated) = state
        .authenticator
        .authenticate(StationTransportAdmissionRequest {
            station_id: &station_id,
            websocket_subprotocol: protocol,
            credential: Some(&credential),
            peer_certificates: &peer.certificates,
        })
    else {
        return authentication_rejection();
    };
    let admission = match state.active.admit(&state.budget, &authenticated.station_id) {
        Ok(admission) => admission,
        Err(StationConnectionAdmissionError::Duplicate) => {
            return rejection(StatusCode::CONFLICT, "station is already connected");
        }
        Err(StationConnectionAdmissionError::Capacity) => {
            return rejection(
                StatusCode::SERVICE_UNAVAILABLE,
                "station capacity exhausted",
            );
        }
    };
    let maximum = state.maximum_message_bytes;
    let Ok(connection_permit) = state.connections.clone().try_reserve_owned() else {
        return rejection(
            StatusCode::SERVICE_UNAVAILABLE,
            "station handoff unavailable",
        );
    };
    ws.protocols([protocol])
        .max_frame_size(maximum)
        .max_message_size(maximum)
        .on_upgrade(move |socket| async move {
            connection_permit.send(StationConnection {
                station: authenticated,
                socket,
                _admission: admission,
            });
        })
}

fn select_protocol(ws: &WebSocketUpgrade) -> Option<&'static str> {
    SUPPORTED_PROTOCOLS.into_iter().find(|supported| {
        ws.requested_protocols()
            .any(|requested| requested.as_bytes() == supported.as_bytes())
    })
}

fn basic_credential(headers: &HeaderMap, station_id: &StationId) -> Option<Vec<u8>> {
    let authorization = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = authorization.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = STANDARD.decode(encoded).ok()?;
    let separator = decoded.iter().position(|byte| *byte == b':')?;
    if decoded[..separator] != station_id.as_str().as_bytes()[..] || separator + 1 == decoded.len()
    {
        return None;
    }
    Some(decoded[separator + 1..].to_vec())
}

fn authentication_rejection() -> Response {
    let mut response = rejection(StatusCode::UNAUTHORIZED, GENERIC_AUTHENTICATION_FAILURE);
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"ocpp\", charset=\"UTF-8\""),
    );
    response
}

fn rejection(status: StatusCode, message: &'static str) -> Response {
    (status, message).into_response()
}

#[derive(Clone, Default)]
struct ActiveStations(Arc<Mutex<BTreeSet<StationId>>>);

impl ActiveStations {
    fn admit(
        &self,
        budget: &RuntimeResourceBudget,
        station_id: &StationId,
    ) -> Result<ActiveStationAdmission, StationConnectionAdmissionError> {
        let mut stations = lock(&self.0);
        if stations.contains(station_id) {
            return Err(StationConnectionAdmissionError::Duplicate);
        }
        let resource = budget
            .admit_station()
            .map_err(|_| StationConnectionAdmissionError::Capacity)?;
        stations.insert(station_id.clone());
        Ok(ActiveStationAdmission {
            active: self.clone(),
            station_id: station_id.clone(),
            _resource: resource,
        })
    }
}

struct ActiveStationAdmission {
    active: ActiveStations,
    station_id: StationId,
    _resource: StationAdmission,
}

impl Drop for ActiveStationAdmission {
    fn drop(&mut self) {
        lock(&self.active.0).remove(&self.station_id);
    }
}

enum StationConnectionAdmissionError {
    Duplicate,
    Capacity,
}

/// Stable invalid endpoint configuration category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StationEndpointConfigurationError {
    /// Accepted-socket handoff capacity must be positive.
    ConnectionCapacity,
}

impl fmt::Display for StationEndpointConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid OCPP endpoint connection capacity")
    }
}

impl Error for StationEndpointConfigurationError {}

/// Failure to start or continue a plaintext OCPP endpoint.
#[derive(Debug)]
pub enum StationEndpointServeError {
    /// Plaintext was requested outside the explicit demo environment.
    PlaintextEnvironment,
    /// The underlying Axum server failed.
    Io(io::Error),
}

impl fmt::Display for StationEndpointServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlaintextEnvironment => {
                formatter.write_str("plaintext OCPP endpoint requires the demo environment")
            }
            Self::Io(error) => write!(formatter, "OCPP endpoint failed: {error}"),
        }
    }
}

impl Error for StationEndpointServeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::PlaintextEnvironment => None,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
