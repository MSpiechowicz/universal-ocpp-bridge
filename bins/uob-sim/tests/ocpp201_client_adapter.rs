#![allow(clippy::result_large_err)]

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use uob_sim::{
    OcppVersion, ProtocolClient, RemoteCommandKind, SimulatorAction, SimulatorCall,
    SimulatorClientConfig, SimulatorProtocolClient,
};

const TEST_BOUND: Duration = Duration::from_secs(4);

fn config(endpoint: String) -> SimulatorClientConfig {
    SimulatorClientConfig {
        endpoint,
        version: OcppVersion::V2_0_1,
        request_timeout: Duration::from_millis(250),
        reconnect: false,
        command_capacity: 8,
        trace_capacity: 32,
        connectors: vec![1],
        evse_connectors: vec![(1, 1), (2, 1), (2, 2)],
    }
}

fn fixture(source: &str, action: SimulatorAction) -> SimulatorCall {
    let frame: Value = serde_json::from_str(source).unwrap();
    assert_eq!(frame[2], action.wire_name(OcppVersion::V2_0_1));
    SimulatorCall {
        action,
        payload: frame[3].clone(),
    }
}

fn charging_calls() -> Vec<SimulatorCall> {
    vec![
        fixture(
            include_str!("fixtures/ocpp201/boot.json"),
            SimulatorAction::BootNotification,
        ),
        fixture(
            include_str!("fixtures/ocpp201/authorize.json"),
            SimulatorAction::Authorize,
        ),
        fixture(
            include_str!("fixtures/ocpp201/status.json"),
            SimulatorAction::StatusNotification,
        ),
        fixture(
            include_str!("fixtures/ocpp201/start.json"),
            SimulatorAction::StartTransaction,
        ),
        fixture(
            include_str!("fixtures/ocpp201/meter.json"),
            SimulatorAction::MeterValues,
        ),
        fixture(
            include_str!("fixtures/ocpp201/stop.json"),
            SimulatorAction::StopTransaction,
        ),
    ]
}

#[tokio::test]
async fn real_socket_preserves_multi_evse_fixtures_and_separates_remote_outcomes() {
    let calls = charging_calls();
    let expected = calls.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept(tcp).await;
        for expected_call in expected.iter().take(5) {
            let observed = receive_json(&mut socket).await;
            assert_eq!(
                observed[2],
                expected_call.action.wire_name(OcppVersion::V2_0_1)
            );
            assert_eq!(observed[3], expected_call.payload);
            let response = match expected_call.action {
                SimulatorAction::BootNotification => json!({
                    "currentTime": "2026-09-01T00:00:00Z",
                    "interval": 60,
                    "status": "Accepted"
                }),
                SimulatorAction::Authorize | SimulatorAction::StartTransaction => {
                    json!({"idTokenInfo": {"status": "Accepted"}})
                }
                SimulatorAction::StatusNotification | SimulatorAction::MeterValues => json!({}),
                SimulatorAction::StopTransaction => unreachable!(),
            };
            reply(&mut socket, &observed, response).await;
        }

        send_remote(
            &mut socket,
            "remote-stop",
            "RequestStopTransaction",
            json!({"transactionId": "tx-201-42"}),
        )
        .await;
        assert_eq!(receive_json(&mut socket).await[2]["status"], "Accepted");

        let stopped = receive_json(&mut socket).await;
        assert_eq!(stopped[2], "TransactionEvent");
        assert_eq!(stopped[3], expected[5].payload);
        reply(&mut socket, &stopped, json!({})).await;

        send_remote(
            &mut socket,
            "remote-stale",
            "RequestStopTransaction",
            json!({"transactionId": "tx-201-42"}),
        )
        .await;
        assert_eq!(receive_json(&mut socket).await[2]["status"], "Rejected");

        send_remote(
            &mut socket,
            "remote-unknown-evse",
            "RequestStartTransaction",
            remote_start(99, 70),
        )
        .await;
        assert_eq!(receive_json(&mut socket).await[2]["status"], "Rejected");

        send_remote(
            &mut socket,
            "remote-start",
            "RequestStartTransaction",
            remote_start(1, 71),
        )
        .await;
        assert_eq!(receive_json(&mut socket).await[2]["status"], "Accepted");

        send_remote(
            &mut socket,
            "remote-unconfirmed-stop",
            "RequestStopTransaction",
            json!({"transactionId": "not-observed"}),
        )
        .await;
        assert_eq!(receive_json(&mut socket).await[2]["status"], "Rejected");
    });

    let client = SimulatorProtocolClient::connect(config(format!("ws://{address}")))
        .await
        .unwrap();
    for call in calls.iter().take(5).cloned() {
        timeout(TEST_BOUND, client.call(call))
            .await
            .unwrap()
            .unwrap();
    }

    assert_remote(&client, RemoteCommandKind::StopTransaction, true).await;
    timeout(TEST_BOUND, client.call(calls[5].clone()))
        .await
        .unwrap()
        .unwrap();
    assert_remote(&client, RemoteCommandKind::StopTransaction, false).await;
    assert_remote(&client, RemoteCommandKind::StartTransaction, false).await;
    assert_remote(&client, RemoteCommandKind::StartTransaction, true).await;
    assert_remote(&client, RemoteCommandKind::StopTransaction, false).await;

    timeout(TEST_BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

async fn assert_remote(
    client: &SimulatorProtocolClient,
    expected: RemoteCommandKind,
    accepted: bool,
) {
    let command = timeout(TEST_BOUND, client.next_remote_command())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.kind, expected);
    assert_eq!(command.accepted, accepted);
}

fn remote_start(evse_id: i64, remote_start_id: i64) -> Value {
    json!({
        "evseId": evse_id,
        "remoteStartId": remote_start_id,
        "idToken": {"idToken": "REMOTE-USER", "type": "Central"}
    })
}

async fn send_remote(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    id: &str,
    action: &str,
    payload: Value,
) {
    socket
        .send(Message::text(json!([2, id, action, payload]).to_string()))
        .await
        .unwrap();
}

async fn reply(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    request: &Value,
    payload: Value,
) {
    socket
        .send(Message::text(json!([3, request[1], payload]).to_string()))
        .await
        .unwrap();
}

async fn receive_json(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> Value {
    let message = timeout(TEST_BOUND, socket.next())
        .await
        .expect("WebSocket read exceeded test bound")
        .expect("peer closed before expected frame")
        .expect("WebSocket frame failed");
    match message {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn accept(
    tcp: tokio::net::TcpStream,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    tokio_tungstenite::accept_hdr_async(
        tcp,
        |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
         mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "ocpp2.0.1".parse().unwrap());
            Ok(response)
        },
    )
    .await
    .unwrap()
}
