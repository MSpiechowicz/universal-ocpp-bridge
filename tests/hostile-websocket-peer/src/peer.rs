use std::collections::VecDeque;
use std::error::Error;
use std::fmt::{Display, Formatter};

use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Message, Utf8Bytes};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub endpoint: String,
    pub subprotocol: String,
    pub max_outbound_bytes: usize,
    pub max_inbound_bytes: usize,
    pub observation_capacity: usize,
    /// Optional prebuilt authorization value for authenticated system-under-test endpoints.
    pub authorization: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Sent,
    Received,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationKind {
    Connected,
    Text,
    Binary,
    Close,
    Error,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Observation {
    pub direction: Direction,
    pub kind: ObservationKind,
    pub bytes: usize,
    pub sha256: Option<String>,
}

#[derive(Debug)]
pub enum PeerError {
    InvalidConfiguration(&'static str),
    Connection(String),
    Protocol(String),
    OutboundLimit { bytes: usize, limit: usize },
    Closed,
}

impl Display for PeerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => formatter.write_str(message),
            Self::Connection(message) => {
                write!(formatter, "WebSocket connection failed: {message}")
            }
            Self::Protocol(message) => write!(formatter, "WebSocket operation failed: {message}"),
            Self::OutboundLimit { bytes, limit } => {
                write!(
                    formatter,
                    "outbound message has {bytes} bytes; configured limit is {limit}"
                )
            }
            Self::Closed => {
                formatter.write_str("WebSocket peer closed before another frame arrived")
            }
        }
    }
}

impl Error for PeerError {}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Peer {
    socket: Socket,
    max_outbound_bytes: usize,
    observations: VecDeque<Observation>,
    observation_capacity: usize,
}

impl Peer {
    /// Opens a bare WebSocket and negotiates exactly the configured OCPP subprotocol.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds, handshake failure, or a mismatched subprotocol.
    pub async fn connect(config: PeerConfig) -> Result<Self, PeerError> {
        if config.max_outbound_bytes == 0
            || config.max_inbound_bytes == 0
            || config.observation_capacity == 0
        {
            return Err(PeerError::InvalidConfiguration(
                "peer bounds must be greater than zero",
            ));
        }
        let mut request = config
            .endpoint
            .into_client_request()
            .map_err(|error| PeerError::Connection(error.to_string()))?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            config
                .subprotocol
                .parse()
                .map_err(|_| PeerError::InvalidConfiguration("invalid WebSocket subprotocol"))?,
        );
        if let Some(authorization) = config.authorization {
            request.headers_mut().insert(
                AUTHORIZATION,
                authorization
                    .parse()
                    .map_err(|_| PeerError::InvalidConfiguration("invalid authorization header"))?,
            );
        }
        let mut websocket_config = WebSocketConfig::default();
        websocket_config.max_message_size = Some(config.max_inbound_bytes);
        websocket_config.max_frame_size = Some(config.max_inbound_bytes);
        let (socket, response) =
            tokio_tungstenite::connect_async_with_config(request, Some(websocket_config), false)
                .await
                .map_err(|error| PeerError::Connection(error.to_string()))?;
        let selected = response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok());
        if selected != Some(config.subprotocol.as_str()) {
            return Err(PeerError::Connection(
                "server selected an unexpected subprotocol".to_owned(),
            ));
        }
        let mut peer = Self {
            socket,
            max_outbound_bytes: config.max_outbound_bytes,
            observations: VecDeque::with_capacity(config.observation_capacity),
            observation_capacity: config.observation_capacity,
        };
        peer.observe(Direction::Received, ObservationKind::Connected, &[]);
        Ok(peer)
    }

    /// Sends an arbitrary text payload without JSON or OCPP validation.
    ///
    /// # Errors
    ///
    /// Returns an error when the peer's own safety bound or WebSocket send fails.
    pub async fn send_text(&mut self, payload: String) -> Result<(), PeerError> {
        self.check_outbound(payload.len())?;
        self.observe(Direction::Sent, ObservationKind::Text, payload.as_bytes());
        self.socket
            .send(Message::Text(Utf8Bytes::from(payload)))
            .await
            .map_err(|error| PeerError::Protocol(error.to_string()))
    }

    /// Sends arbitrary binary bytes without protocol-model validation.
    ///
    /// # Errors
    ///
    /// Returns an error when the peer's own safety bound or WebSocket send fails.
    pub async fn send_binary(&mut self, payload: Vec<u8>) -> Result<(), PeerError> {
        self.check_outbound(payload.len())?;
        self.observe(Direction::Sent, ObservationKind::Binary, &payload);
        self.socket
            .send(Message::Binary(Bytes::from(payload)))
            .await
            .map_err(|error| PeerError::Protocol(error.to_string()))
    }

    /// Receives the next data, close, or transport-error outcome.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket ends without a close frame or the WebSocket read fails.
    pub async fn receive(&mut self) -> Result<Message, PeerError> {
        match self.socket.next().await {
            Some(Ok(message)) => {
                let (kind, bytes) = message_summary(&message);
                self.observe(Direction::Received, kind, bytes);
                Ok(message)
            }
            Some(Err(error)) => {
                self.observe(Direction::Received, ObservationKind::Error, &[]);
                Err(PeerError::Protocol(error.to_string()))
            }
            None => Err(PeerError::Closed),
        }
    }

    /// Completes a normal WebSocket close handshake.
    ///
    /// # Errors
    ///
    /// Returns an error if the close frame cannot be sent.
    pub async fn disconnect(&mut self) -> Result<(), PeerError> {
        self.socket
            .close(None)
            .await
            .map_err(|error| PeerError::Protocol(error.to_string()))?;
        self.observe(Direction::Sent, ObservationKind::Disconnected, &[]);
        Ok(())
    }

    #[must_use]
    pub fn observations(&self) -> Vec<Observation> {
        self.observations.iter().cloned().collect()
    }

    fn check_outbound(&self, bytes: usize) -> Result<(), PeerError> {
        if bytes > self.max_outbound_bytes {
            Err(PeerError::OutboundLimit {
                bytes,
                limit: self.max_outbound_bytes,
            })
        } else {
            Ok(())
        }
    }

    fn observe(&mut self, direction: Direction, kind: ObservationKind, payload: &[u8]) {
        if self.observations.len() == self.observation_capacity {
            self.observations.pop_front();
        }
        self.observations.push_back(Observation {
            direction,
            kind,
            bytes: payload.len(),
            sha256: (!payload.is_empty()).then(|| sha256_hex(payload)),
        });
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for &byte in &digest {
        encoded.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn message_summary(message: &Message) -> (ObservationKind, &[u8]) {
    match message {
        Message::Text(value) => (ObservationKind::Text, value.as_bytes()),
        Message::Binary(value) => (ObservationKind::Binary, value.as_ref()),
        Message::Close(_) => (ObservationKind::Close, &[]),
        _ => (ObservationKind::Binary, &[]),
    }
}
