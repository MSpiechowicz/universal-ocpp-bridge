//! OCPP 1.6 peer socket and client configuration shared by the trigger narratives.
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use uob_sim::{OcppVersion, SimulatorClientConfig, TriggerObservation, TriggerResponses};

pub(crate) const BOUND: Duration = Duration::from_secs(4);
pub(crate) type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

pub(crate) fn config(endpoint: String) -> SimulatorClientConfig {
    SimulatorClientConfig {
        endpoint,
        credentials_file: None,
        version: OcppVersion::V1_6,
        request_timeout: Duration::from_secs(2),
        reconnect: false,
        command_capacity: 16,
        trace_capacity: 64,
        connectors: vec![1, 2],
        evse_connectors: vec![],
        trigger_responses: TriggerResponses::default(),
        trigger_observation: TriggerObservation::default(),
        local_authorization: None,
        local_authorization_file: None,
        reservation16: None,
        reservation201: None,
        firmware16: None,
        firmware201: None,
    }
}

pub(crate) struct Ocpp16Handshake;

impl Callback for Ocpp16Handshake {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
        Ok(response)
    }
}

pub(crate) async fn accept(listener: &TcpListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_hdr_async(stream, Ocpp16Handshake)
        .await
        .unwrap()
}

pub(crate) async fn recv(socket: &mut Socket) -> Value {
    let frame = timeout(BOUND, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match frame {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        frame => panic!("unexpected WebSocket frame: {frame:?}"),
    }
}
