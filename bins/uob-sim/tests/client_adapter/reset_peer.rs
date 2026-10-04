use super::{Arc, Mutex, TEST_BOUND, TcpListener, Value, receive_json, timeout};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
pub(super) async fn websocket_server(
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
    if expected_protocol == "ocpp1.6" {
        let (tcp, _) = timeout(TEST_BOUND, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut recovered = tokio_tungstenite::accept_hdr_async(
            tcp, |_: &tokio_tungstenite::tungstenite::handshake::server::Request,
                mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                response.headers_mut().insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
                Ok(response)
            },
        ).await.unwrap();
        let boot = receive_json(&mut recovered).await;
        assert_eq!(boot[2], "BootNotification");
        recovered
            .send(Message::text(
                json!([3,boot[1],{
                    "status":"Accepted","interval":60,"currentTime":"2026-09-01T00:00:00Z"
                }])
                .to_string(),
            ))
            .await
            .unwrap();
        // Keep this socket alive until the round-trip test explicitly shuts down.
        tokio::spawn(async move { while recovered.next().await.is_some() {} });
    }

    let protocol = offered.lock().unwrap().clone();
    (protocol, reset_response)
}
