use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use uob_hostile_websocket_peer::{
    Direction, ObservationKind, Peer, PeerConfig, ScenarioRunner, parse_scenario,
};

type ServerSocket = WebSocketStream<TcpStream>;

async fn accept(listener: &TcpListener) -> ServerSocket {
    let (tcp, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_hdr_async(
        tcp,
        |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
         mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
            Ok(response)
        },
    )
    .await
    .unwrap()
}

async fn text(socket: &mut ServerSocket) -> String {
    match timeout(Duration::from_secs(1), socket.next())
        .await
        .expect("server read exceeded test bound")
        .expect("peer closed before expected frame")
        .expect("WebSocket read failed")
    {
        Message::Text(value) => value.to_string(),
        other => panic!("expected text frame, got {other:?}"),
    }
}

fn endpoint(listener: &TcpListener) -> String {
    format!("ws://{}", listener.local_addr().unwrap())
}

#[tokio::test]
async fn malformed_json_and_invalid_schema_reach_the_socket_unchanged() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let scenario_endpoint = endpoint(&listener);
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(text(&mut socket).await, "[2,\"broken\"");
        socket
            .send(Message::Text(Utf8Bytes::from_static(
                r#"[4,"broken","FormationViolation","invalid JSON",{"path":"/"}]"#,
            )))
            .await
            .unwrap();
        assert_eq!(
            text(&mut socket).await,
            r#"[2,"invalid-schema","MeterValues",{"meterValue":"wrong-type"}]"#
        );
        socket
            .send(Message::Text(Utf8Bytes::from_static(
                r#"[4,"invalid-schema","FormationViolation","schema error",{"path":"/meterValue"}]"#,
            )))
            .await
            .unwrap();
        socket.close(None).await.unwrap();
    });
    let scenario = parse_scenario(&format!(
        r#"
schema_version = 1
endpoint = "{scenario_endpoint}"
subprotocol = "ocpp1.6"

[[steps]]
id = "connect"
action = "connect"

[[steps]]
id = "malformed"
action = "send"
[steps.payload]
kind = "text"
value = '[2,"broken"'

[[steps]]
id = "malformed-error"
action = "receive"
[steps.expect]
kind = "text"
json_pointer = "/4/path"
equals = "/"

[[steps]]
id = "invalid-schema"
action = "send"
[steps.payload]
kind = "text"
value = '[2,"invalid-schema","MeterValues",{{"meterValue":"wrong-type"}}]'

[[steps]]
id = "schema-error"
action = "receive"
[steps.expect]
kind = "text"
json_pointer = "/4/path"
equals = "/meterValue"
"#,
    ))
    .unwrap();

    let report = ScenarioRunner.run(&scenario).await;
    assert_eq!(report.failure, None);
    assert_eq!(report.steps.len(), 5);
    assert!(report.observations.iter().all(|observation| {
        observation
            .sha256
            .as_ref()
            .is_none_or(|hash| hash.len() == 64)
    }));
    server.await.unwrap();
}

#[tokio::test]
async fn generated_payloads_exercise_just_below_and_above_a_bridge_bound() {
    const BRIDGE_LIMIT: usize = 32;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let scenario_endpoint = endpoint(&listener);
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(text(&mut socket).await.len(), BRIDGE_LIMIT - 1);
        socket
            .send(Message::Text(Utf8Bytes::from_static(
                r#"{"status":"continued"}"#,
            )))
            .await
            .unwrap();
        assert_eq!(text(&mut socket).await.len(), BRIDGE_LIMIT + 1);
        socket
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Size,
                reason: Utf8Bytes::from_static("message too large"),
            })))
            .await
            .unwrap();
    });
    let scenario = parse_scenario(&format!(
        r#"
schema_version = 1
endpoint = "{scenario_endpoint}"
subprotocol = "ocpp1.6"
max_outbound_bytes = 64

[[steps]]
id = "connect"
action = "connect"

[[steps]]
id = "below-limit"
action = "send"
[steps.payload]
kind = "repeated_text"
byte = "a"
bytes = 31

[[steps]]
id = "continued-service"
action = "receive"
[steps.expect]
kind = "text"
json_pointer = "/status"
equals = "continued"

[[steps]]
id = "above-limit"
action = "send"
[steps.payload]
kind = "repeated_text"
byte = "b"
bytes = 33

[[steps]]
id = "closed-at-bound"
action = "receive"
[steps.expect]
kind = "close"
code = 1009
"#
    ))
    .unwrap();

    let report = ScenarioRunner.run(&scenario).await;
    assert_eq!(report.failure, None);
    server.await.unwrap();
}

#[tokio::test]
async fn duplicate_unmatched_and_out_of_order_messages_do_not_hang_another_station() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let scenario_endpoint = endpoint(&listener);
    let server = tokio::spawn(async move {
        let (first, second) = tokio::join!(accept(&listener), accept(&listener));
        let handle = |mut socket: ServerSocket| async move {
            let first = text(&mut socket).await;
            if first.contains("missing") {
                let duplicate = text(&mut socket).await;
                assert!(duplicate.contains("hostile"));
                assert_eq!(text(&mut socket).await, duplicate);
                socket
                    .send(Message::Text(Utf8Bytes::from_static(
                        r#"[3,"hostile",{"order":2}]"#,
                    )))
                    .await
                    .unwrap();
                socket
                    .send(Message::Text(Utf8Bytes::from_static(
                        r#"[3,"missing",{"order":1}]"#,
                    )))
                    .await
                    .unwrap();
            } else {
                assert!(first.contains("healthy"));
                socket
                    .send(Message::Text(Utf8Bytes::from_static(r#"[3,"healthy",{}]"#)))
                    .await
                    .unwrap();
            }
        };
        tokio::join!(handle(first), handle(second));
    });
    let hostile = parse_scenario(&sequence_scenario(&scenario_endpoint, true)).unwrap();
    let healthy = parse_scenario(&sequence_scenario(&scenario_endpoint, false)).unwrap();
    let runs = Box::pin(timeout(Duration::from_secs(2), async {
        tokio::join!(ScenarioRunner.run(&hostile), ScenarioRunner.run(&healthy))
    }))
    .await
    .expect("concurrent peers hung");

    assert_eq!(runs.0.failure, None);
    assert_eq!(runs.1.failure, None);
    server.await.unwrap();
}

#[tokio::test]
async fn observations_are_bounded_and_store_digests_instead_of_payloads() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_endpoint = endpoint(&listener);
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let _ = text(&mut socket).await;
        let _ = text(&mut socket).await;
    });
    let mut peer = Peer::connect(PeerConfig {
        endpoint: peer_endpoint,
        subprotocol: "ocpp1.6".to_owned(),
        max_outbound_bytes: 128,
        max_inbound_bytes: 128,
        observation_capacity: 2,
    })
    .await
    .unwrap();
    peer.send_text("private-payload-one".to_owned())
        .await
        .unwrap();
    peer.send_text("private-payload-two".to_owned())
        .await
        .unwrap();

    let observations = peer.observations();
    assert_eq!(observations.len(), 2);
    assert!(
        observations
            .iter()
            .all(|item| item.direction == Direction::Sent)
    );
    assert!(
        observations
            .iter()
            .all(|item| item.kind == ObservationKind::Text)
    );
    assert!(
        observations
            .iter()
            .all(|item| item.sha256.as_ref().is_some_and(|hash| hash.len() == 64))
    );
    server.await.unwrap();
}

fn sequence_scenario(endpoint: &str, hostile: bool) -> String {
    let (payloads, receives) = if hostile {
        (
            r#"
[[steps]]
id = "unmatched"
action = "send"
[steps.payload]
kind = "text"
value = '[3,"missing",{}]'
[[steps]]
id = "duplicate-one"
action = "send"
[steps.payload]
kind = "text"
value = '[2,"hostile","Heartbeat",{}]'
[[steps]]
id = "duplicate-two"
action = "send"
[steps.payload]
kind = "text"
value = '[2,"hostile","Heartbeat",{}]'
"#,
            r#"
[[steps]]
id = "response-two"
action = "receive"
[steps.expect]
kind = "text"
json_pointer = "/1"
equals = "hostile"
[[steps]]
id = "response-one"
action = "receive"
[steps.expect]
kind = "text"
json_pointer = "/1"
equals = "missing"
"#,
        )
    } else {
        (
            r#"
[[steps]]
id = "healthy-call"
action = "send"
[steps.payload]
kind = "text"
value = '[2,"healthy","Heartbeat",{}]'
"#,
            r#"
[[steps]]
id = "healthy-response"
action = "receive"
[steps.expect]
kind = "text"
json_pointer = "/1"
equals = "healthy"
"#,
        )
    };
    format!(
        r#"
schema_version = 1
endpoint = "{endpoint}"
subprotocol = "ocpp1.6"
[[steps]]
id = "connect"
action = "connect"
{payloads}
{receives}
"#
    )
}
