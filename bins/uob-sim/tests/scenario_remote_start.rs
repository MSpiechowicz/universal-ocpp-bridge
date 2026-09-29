#![allow(clippy::result_large_err)] // Tungstenite's handshake callback fixes the error type.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use uob_sim::scenario::{ScenarioRunner, cancellation_pair, parse_configuration, parse_scenario};

#[tokio::test]
async fn awaited_201_remote_start_id_is_sent_on_the_real_transaction_event() {
    let report = remote_start_scenario_over_socket(1).await;
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

#[tokio::test]
async fn rejected_201_remote_start_cannot_supply_a_transaction_id() {
    let report = remote_start_scenario_over_socket(9).await;
    assert_eq!(report.failure.unwrap().code, "remote_start_id_unavailable");
}

#[test]
fn bound_remote_start_rejects_an_explicit_id_instead_of_overwriting_it() {
    let source = remote_start_scenario().replace(
        "transactionInfo = { transactionId = \"tx-bound\", chargingState = \"Charging\" }",
        "transactionInfo = { transactionId = \"tx-bound\", chargingState = \"Charging\", remoteStartId = 123 }",
    );
    assert_eq!(
        parse_scenario(&source).unwrap_err().code,
        "invalid_remote_start_binding"
    );
}

async fn remote_start_scenario_over_socket(evse_id: u16) -> uob_sim::scenario::RunReport {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            tcp,
            |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
             mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    "ocpp2.0.1".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        for action in ["BootNotification", "Authorize"] {
            let call = receive_frame(&mut socket).await;
            assert_eq!(call[2], action);
            let response = if action == "BootNotification" {
                serde_json::json!({"status": "Accepted", "interval": 60, "currentTime": "2026-09-01T00:00:00Z"})
            } else {
                serde_json::json!({"idTokenInfo": {"status": "Accepted"}})
            };
            socket
                .send(Message::text(
                    serde_json::json!([3, call[1], response]).to_string(),
                ))
                .await
                .unwrap();
        }

        socket
            .send(Message::text(
                serde_json::json!([2, "remote-id", "RequestStartTransaction", {
                    "evseId": evse_id,
                    "remoteStartId": 8123,
                    "idToken": {"idToken": "LOCAL-USER", "type": "Central"}
                }])
                .to_string(),
            ))
            .await
            .unwrap();
        let reply = receive_frame(&mut socket).await;
        assert_eq!(reply[0], 3);
        assert_eq!(reply[1], "remote-id");
        assert_eq!(
            reply[2]["status"],
            if evse_id == 1 { "Accepted" } else { "Rejected" }
        );

        if evse_id == 1 {
            let event = receive_frame(&mut socket).await;
            assert_eq!(event[2], "TransactionEvent");
            assert_eq!(event[3]["eventType"], "Started");
            assert_eq!(event[3]["transactionInfo"]["remoteStartId"], 8123);
            socket
                .send(Message::text(
                    serde_json::json!([3, event[1], {"idTokenInfo": {"status": "Accepted"}}])
                        .to_string(),
                ))
                .await
                .unwrap();
        }
    });
    let configuration = parse_configuration(&format!(
        "schema_version = 1\n[[stations]]\nid = \"alpha\"\nendpoint = \"{endpoint}\"\nocpp_version = \"2.0.1\"\nrequest_timeout_ms = 1000\n[[stations.evses]]\nid = 1\nconnectors = [1]\n"
    ))
    .unwrap();
    let scenario = parse_scenario(remote_start_scenario()).unwrap();
    let report = timeout(
        Duration::from_secs(4),
        ScenarioRunner::default().run(
            &configuration,
            &scenario,
            scenario.seed,
            cancellation_pair().1,
        ),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(4), server)
        .await
        .unwrap()
        .unwrap();
    report
}

async fn receive_frame(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> serde_json::Value {
    let message = timeout(Duration::from_secs(4), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match message {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    }
}

fn remote_start_scenario() -> &'static str {
    r#"
schema_version = 1
seed = 15
[[steps]]
id = "connect"
station = "alpha"
action = "connect"
timeout_ms = 2000
[[steps]]
id = "boot"
station = "alpha"
action = "boot"
timeout_ms = 2000
fixture_id = "sim.ocpp201.boot.accepted"
payload = { chargingStation = { vendorName = "UOB", model = "Simulator" }, reason = "PowerUp" }
[[steps]]
id = "authorize"
station = "alpha"
action = "authorize"
timeout_ms = 2000
fixture_id = "sim.ocpp201.authorize.accepted"
payload = { idToken = { idToken = "LOCAL-USER", type = "Central" } }
[[steps]]
id = "remote-start"
station = "alpha"
action = "await_remote_start"
timeout_ms = 2000
[[steps]]
id = "start"
station = "alpha"
action = "start_transaction"
timeout_ms = 2000
fixture_id = "sim.ocpp201.transaction.start"
use_awaited_remote_start_id = true
payload = { eventType = "Started", timestamp = "2026-09-01T00:00:01Z", triggerReason = "RemoteStart", seqNo = 0, transactionInfo = { transactionId = "tx-bound", chargingState = "Charging" }, evse = { id = 1, connectorId = 1 }, idToken = { idToken = "LOCAL-USER", type = "Central" } }
"#
}
