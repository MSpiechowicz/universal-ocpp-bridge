use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use uob_sim::scenario::{ScenarioRunner, cancellation_pair, parse_configuration, parse_scenario};

struct Ocpp201Handshake;
impl Callback for Ocpp201Handshake {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp2.0.1".parse().unwrap());
        Ok(response)
    }
}

async fn recv(socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
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

#[tokio::test]
async fn triggered_boot_unlocks_later_scenario_201_status_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let configuration = parse_configuration(&format!(
        "schema_version = 1\n[[stations]]\nid = \"alpha\"\nendpoint = \"ws://{}\"\nocpp_version = \"2.0.1\"\n[[stations.evses]]\nid = 2\nconnectors = [1]\n",
        listener.local_addr().unwrap()
    )).unwrap();
    let scenario = parse_scenario(r#"
schema_version = 1
seed = 116
[[steps]]
id = "connect"
station = "alpha"
action = "connect"
timeout_ms = 1000
[[steps]]
id = "wait-for-trigger"
station = "alpha"
action = "wait"
duration_ms = 200
timeout_ms = 1000
[[steps]]
id = "status"
station = "alpha"
action = "status"
timeout_ms = 1000
fixture_id = "sim.ocpp201.status.available"
payload = { evseId = 2, connectorId = 1, connectorStatus = "Available", timestamp = "2026-09-01T00:00:01Z" }
expect_response = {}
[[steps]]
id = "disconnect"
station = "alpha"
action = "disconnect"
timeout_ms = 1000
"#).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, Ocpp201Handshake)
            .await
            .unwrap();
        socket
            .send(Message::text(
                json!([2,"trigger","TriggerMessage",{"requestedMessage":"BootNotification"}])
                    .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            recv(&mut socket).await,
            json!([3,"trigger",{"status":"Accepted"}])
        );
        let boot = recv(&mut socket).await;
        assert_eq!(boot[2], "BootNotification");
        assert_eq!(boot[3]["reason"], "Triggered");
        socket.send(Message::text(json!([3,boot[1],{"currentTime":"2026-09-01T00:00:00Z","interval":60,"status":"Accepted"}]).to_string())).await.unwrap();
        let status = recv(&mut socket).await;
        assert_eq!(status[2], "StatusNotification");
        assert_eq!(
            (
                status[3]["evseId"].as_u64(),
                status[3]["connectorId"].as_u64()
            ),
            (Some(2), Some(1))
        );
        socket
            .send(Message::text(json!([3, status[1], {}]).to_string()))
            .await
            .unwrap();
        let _ = socket.close(None).await;
    });
    let report = ScenarioRunner::default()
        .run(
            &configuration,
            &scenario,
            scenario.seed,
            cancellation_pair().1,
        )
        .await;
    assert!(report.failure.is_none(), "{:?}", report.failure);
    timeout(Duration::from_secs(4), server)
        .await
        .unwrap()
        .unwrap();
}
