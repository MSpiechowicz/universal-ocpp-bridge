use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use uob_sim::{
    OcppVersion, ProtocolClient, SimulatorAction, SimulatorCall, SimulatorClientConfig,
    SimulatorProtocolClient, TriggerObservation, TriggerResponses,
};

struct Ocpp16Handshake;
impl Callback for Ocpp16Handshake {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
        Ok(response)
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
async fn recv(socket: &mut Socket) -> Value {
    match timeout(Duration::from_secs(4), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("unexpected WebSocket frame: {other:?}"),
    }
}

fn config(listener: &TcpListener, reconnect: bool) -> SimulatorClientConfig {
    SimulatorClientConfig {
        endpoint: format!("ws://{}", listener.local_addr().unwrap()),
        credentials_file: None,
        version: OcppVersion::V1_6,
        request_timeout: Duration::from_secs(2),
        reconnect,
        command_capacity: 16,
        trace_capacity: 64,
        connectors: vec![1],
        evse_connectors: vec![],
        trigger_responses: TriggerResponses::default(),
        trigger_observation: TriggerObservation::default(),
        local_authorization: None,
        local_authorization_file: None,
        reservation16: None,
        reservation201: None,
        firmware16: None,
        firmware201: None,
        diagnostics16: None,
        diagnostics201: None,
    }
}

async fn accept(listener: &TcpListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_hdr_async(stream, Ocpp16Handshake)
        .await
        .unwrap()
}

async fn trigger_boot(socket: &mut Socket, id: &str) {
    socket
        .send(Message::text(
            json!([2,id,"TriggerMessage",{"requestedMessage":"BootNotification"}]).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(recv(socket).await, json!([3,id,{"status":"Accepted"}]));
    let boot = recv(socket).await;
    assert_eq!(boot[2], "BootNotification");
    socket
        .send(Message::text(
            json!([3,boot[1],{
                "currentTime":"2026-09-01T00:00:00Z","interval":60,"status":"Accepted"
            }])
            .to_string(),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn malformed_boot_response_does_not_register_station_or_block_triggered_boot() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&listener, false);
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let initial = recv(&mut socket).await;
        assert_eq!(initial[2], "BootNotification");
        socket
            .send(Message::text(
                json!([3, initial[1], {"status":"Accepted"}]).to_string(),
            ))
            .await
            .unwrap();
        trigger_boot(&mut socket, "retry").await;
        socket
            .send(Message::text(
                json!([2,"again","TriggerMessage",{"requestedMessage":"BootNotification"}])
                    .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            recv(&mut socket).await,
            json!([3,"again",{"status":"Rejected"}])
        );
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    assert!(
        client
            .call(SimulatorCall {
                action: SimulatorAction::BootNotification,
                payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
            })
            .await
            .is_err()
    );
    timeout(Duration::from_secs(4), server)
        .await
        .unwrap()
        .unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn accepted_boot_on_previous_socket_does_not_block_boot_trigger_after_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&listener, true);
    let server = tokio::spawn(async move {
        let mut first = accept(&listener).await;
        trigger_boot(&mut first, "first").await;
        // A second Boot on this socket is invalid after an accepted registration.
        first
            .send(Message::text(
                json!([2,"same","TriggerMessage",{"requestedMessage":"BootNotification"}])
                    .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            recv(&mut first).await,
            json!([3,"same",{"status":"Rejected"}])
        );
        first.close(None).await.unwrap();

        let mut second = timeout(Duration::from_secs(4), accept(&listener))
            .await
            .unwrap();
        trigger_boot(&mut second, "reconnected").await;
        second.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    timeout(Duration::from_secs(4), server)
        .await
        .unwrap()
        .unwrap();
    client.shutdown().await.unwrap();
}
