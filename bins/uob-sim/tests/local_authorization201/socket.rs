use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

const BOUND: Duration = Duration::from_secs(5);

pub(crate) async fn accept(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (tcp, _) = timeout(BOUND, listener.accept()).await.unwrap().unwrap();
    tokio_tungstenite::accept_hdr_async(tcp, select_protocol)
        .await
        .unwrap()
}

// Tungstenite fixes this callback's Result/error type even for accepted handshakes.
#[allow(clippy::result_large_err, clippy::unnecessary_wraps)]
fn select_protocol(_: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp2.0.1".parse().unwrap());
    Ok(response)
}
pub(crate) async fn receive(socket: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        let message = timeout(BOUND, socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}
pub(crate) async fn send(socket: &mut WebSocketStream<TcpStream>, frame: Value) {
    socket.send(Message::text(frame.to_string())).await.unwrap();
}
pub(crate) async fn boot(socket: &mut WebSocketStream<TcpStream>) {
    let request = receive(socket).await;
    assert_eq!(request[2], "BootNotification");
    send(socket, json!([3,request[1],{"status":"Accepted","currentTime":"2026-10-04T00:00:00Z","interval":60}])).await;
}
pub(crate) async fn closed(socket: &mut WebSocketStream<TcpStream>) {
    timeout(BOUND, async {
        while let Some(message) = socket.next().await {
            match message.unwrap() {
                Message::Close(_) => return,
                Message::Text(_) => panic!("offline socket transmitted an unexpected native call"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}
