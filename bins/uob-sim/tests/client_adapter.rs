use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use uob_sim::{
    OcppVersion, ProtocolClient, SimulatorClientConfig, SimulatorClientError,
    SimulatorProtocolClient, TraceKind,
};

const TEST_BOUND: Duration = Duration::from_secs(4);

fn config(endpoint: String, version: OcppVersion) -> SimulatorClientConfig {
    SimulatorClientConfig {
        endpoint,
        version,
        request_timeout: Duration::from_millis(120),
        reconnect: false,
        command_capacity: 2,
        trace_capacity: 16,
    }
}

async fn websocket_server(
    listener: TcpListener,
    expected_protocol: &'static str,
) -> (String, Value) {
    let (tcp, _) = listener.accept().await.unwrap();
    let offered = Arc::new(Mutex::new(String::new()));
    let observed = Arc::clone(&offered);
    let mut socket = tokio_tungstenite::accept_hdr_async(
        tcp,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            request
                .headers()
                .get("Sec-WebSocket-Protocol")
                .unwrap()
                .to_str()
                .unwrap()
                .clone_into(&mut observed.lock().unwrap());
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", expected_protocol.parse().unwrap());
            Ok(response)
        },
    )
    .await
    .unwrap();

    let heartbeat = receive_json(&mut socket).await;
    assert_eq!(heartbeat[0], 2);
    assert_eq!(heartbeat[2], "Heartbeat");
    let heartbeat_id = heartbeat[1].as_str().unwrap();
    socket
        .send(Message::text(
            json!([3, heartbeat_id, {"currentTime": "2026-09-01T00:00:00Z"}]).to_string(),
        ))
        .await
        .unwrap();

    let reset_payload = if expected_protocol == "ocpp1.6" {
        json!({"type": "Soft"})
    } else {
        json!({"type": "Immediate"})
    };
    socket
        .send(Message::text(
            json!([2, "server-reset", "Reset", reset_payload]).to_string(),
        ))
        .await
        .unwrap();
    let reset_response = receive_json(&mut socket).await;
    socket.close(None).await.unwrap();

    let protocol = offered.lock().unwrap().clone();
    (protocol, reset_response)
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

async fn assert_round_trip(version: OcppVersion) {
    let protocol = version.websocket_protocol();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(websocket_server(listener, protocol));
    let client = SimulatorProtocolClient::connect(config(format!("ws://{address}"), version))
        .await
        .unwrap();

    let timestamp = timeout(TEST_BOUND, client.heartbeat())
        .await
        .expect("Heartbeat exceeded test bound")
        .unwrap();
    assert_eq!(timestamp, "2026-09-01T00:00:00Z");
    let (offered, reset_response) = timeout(TEST_BOUND, server).await.unwrap().unwrap();

    assert_eq!(offered, protocol);
    assert_eq!(reset_response[0], 3);
    assert_eq!(reset_response[1], "server-reset");
    assert_eq!(reset_response[2]["status"], "Accepted");
    wait_for_trace(&client, TraceKind::ResetReceived).await;
    timeout(TEST_BOUND, client.shutdown())
        .await
        .expect("shutdown exceeded test bound")
        .unwrap();
}

async fn wait_for_trace(client: &SimulatorProtocolClient, expected: TraceKind) {
    timeout(TEST_BOUND, async {
        loop {
            if client.traces().iter().any(|event| event.kind == expected) {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ocpp_1_6_exchanges_calls_and_inbound_commands_on_a_real_socket() {
    assert_round_trip(OcppVersion::V1_6).await;
}

#[tokio::test]
async fn ocpp_2_0_1_exchanges_calls_and_inbound_commands_on_a_real_socket() {
    assert_round_trip(OcppVersion::V2_0_1).await;
}

#[tokio::test]
async fn timeout_cancellation_and_project_buffers_remain_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (first_seen, first_seen_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept(tcp, "ocpp1.6").await;
        let _first = receive_json(&mut socket).await;
        let _ = first_seen.send(());
        let _second = receive_json(&mut socket).await;
        sleep(Duration::from_millis(300)).await;
    });
    let mut client_config = config(format!("ws://{address}"), OcppVersion::V1_6);
    client_config.request_timeout = Duration::from_millis(80);
    client_config.command_capacity = 1;
    client_config.trace_capacity = 3;
    let client = SimulatorProtocolClient::connect(client_config)
        .await
        .unwrap();

    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.heartbeat().await });
    timeout(TEST_BOUND, first_seen_rx).await.unwrap().unwrap();
    first.abort();

    let second_client = client.clone();
    let second = tokio::spawn(async move { second_client.heartbeat().await });
    tokio::task::yield_now().await;
    assert!(matches!(
        timeout(Duration::from_millis(100), client.heartbeat())
            .await
            .expect("full queue did not reject within its bound"),
        Err(SimulatorClientError::QueueFull)
    ));

    let second_result = timeout(TEST_BOUND, second).await.unwrap().unwrap();
    assert!(matches!(
        second_result,
        Err(SimulatorClientError::Protocol(_))
    ));
    assert!(client.traces().len() <= 3);
    let diagnostics = client.diagnostics();
    assert_eq!(diagnostics.rejected_commands, 1);
    assert!(diagnostics.dropped_traces > 0);
    timeout(TEST_BOUND, client.shutdown())
        .await
        .unwrap()
        .unwrap();
    timeout(TEST_BOUND, server).await.unwrap().unwrap();
}

#[tokio::test]
async fn reconnect_preserves_handlers_and_allows_a_later_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (connections, mut connection_events) = mpsc::channel(2);
    let server = tokio::spawn(async move {
        let (first_tcp, _) = listener.accept().await.unwrap();
        let mut first = accept(first_tcp, "ocpp2.0.1").await;
        connections.send(()).await.unwrap();
        first.close(None).await.unwrap();

        let (second_tcp, _) = listener.accept().await.unwrap();
        let mut second = accept(second_tcp, "ocpp2.0.1").await;
        connections.send(()).await.unwrap();
        let call = receive_json(&mut second).await;
        second
            .send(Message::text(
                json!([3, call[1], {"currentTime": "2026-09-01T01:00:00Z"}]).to_string(),
            ))
            .await
            .unwrap();
    });
    let mut client_config = config(format!("ws://{address}"), OcppVersion::V2_0_1);
    client_config.reconnect = true;
    let client = SimulatorProtocolClient::connect(client_config)
        .await
        .unwrap();

    timeout(TEST_BOUND, connection_events.recv())
        .await
        .unwrap()
        .unwrap();
    timeout(TEST_BOUND, connection_events.recv())
        .await
        .unwrap()
        .unwrap();
    wait_for_trace(&client, TraceKind::Reconnected).await;
    assert_eq!(
        timeout(TEST_BOUND, client.heartbeat())
            .await
            .unwrap()
            .unwrap(),
        "2026-09-01T01:00:00Z"
    );
    timeout(TEST_BOUND, client.shutdown())
        .await
        .unwrap()
        .unwrap();
    timeout(TEST_BOUND, server).await.unwrap().unwrap();
}

#[tokio::test]
async fn bounded_outstanding_calls_correlate_out_of_order_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept(tcp, "ocpp1.6").await;
        let first = receive_json(&mut socket).await;
        let second = receive_json(&mut socket).await;
        socket
            .send(Message::text(
                json!([3, second[1], {"currentTime": "2026-09-01T00:00:02Z"}]).to_string(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::text(
                json!([3, first[1], {"currentTime": "2026-09-01T00:00:01Z"}]).to_string(),
            ))
            .await
            .unwrap();
    });
    let client =
        SimulatorProtocolClient::connect(config(format!("ws://{address}"), OcppVersion::V1_6))
            .await
            .unwrap();

    let (first, second) = tokio::join!(client.heartbeat(), client.heartbeat());
    assert_eq!(first.unwrap(), "2026-09-01T00:00:01Z");
    assert_eq!(second.unwrap(), "2026-09-01T00:00:02Z");
    timeout(TEST_BOUND, client.shutdown())
        .await
        .unwrap()
        .unwrap();
    timeout(TEST_BOUND, server).await.unwrap().unwrap();
}

async fn accept(
    tcp: tokio::net::TcpStream,
    protocol: &'static str,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    tokio_tungstenite::accept_hdr_async(
        tcp,
        move |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
            Ok(response)
        },
    )
    .await
    .unwrap()
}
