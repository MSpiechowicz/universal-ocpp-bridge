use super::{STATION_SECRET, protocol::Clock};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::{JoinHandle, JoinSet},
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header::AUTHORIZATION},
};
use uob_application::{CommandClock, LocalAuthorizationService, StationEvent};
use uob_contracts::{ResourceRef, TransactionSnapshot};

pub type Auth = LocalAuthorizationService<Value, StationEvent, TransactionSnapshot, String>;
pub(super) const MAX_PENDING_HANDSHAKES: usize = 16;
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

// tungstenite fixes this callback's rejection type to a large HTTP ErrorResponse.
#[allow(clippy::result_large_err)]
async fn handshake(
    stream: TcpStream,
    capability_path: String,
    protocol: &'static str,
) -> Result<WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error> {
    accept_hdr_async(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let offered = request.headers().get_all("Sec-WebSocket-Protocol");
            if request.uri().path() != capability_path.as_str()
                || request.uri().query().is_some()
                || offered.iter().count() != 1
                || !offered
                    .iter()
                    .next()
                    .is_some_and(|header| header.to_str().is_ok_and(|value| value == protocol))
            {
                return Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("Forbidden".to_owned()))
                    .unwrap());
            }
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
            Ok(response)
        },
    )
    .await
}

pub async fn start(
    upstream: String,
    auth: Arc<Auth>,
    station: ResourceRef,
    protocol: &'static str,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "ws://{}/ocpp/{}/{}",
        listener.local_addr().unwrap(),
        station.station_id.as_str(),
        uuid::Uuid::new_v4()
    );
    let capability_path = reqwest::Url::parse(&endpoint).unwrap().path().to_owned();
    let task = tokio::spawn(async move {
        let pending = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));
        // Dropping the listener task aborts every accepted connection, including idle handshakes.
        let mut children = JoinSet::new();
        loop {
            let (stream, _) = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(peer) => peer,
                    Err(_) => break,
                },
                _ = children.join_next(), if !children.is_empty() => continue,
            };
            let Ok(permit) = pending.clone().try_acquire_owned() else {
                // No task, or retained socket, for a peer beyond the handshake limit.
                continue;
            };
            let upstream = upstream.clone();
            let auth = auth.clone();
            let capability_path = capability_path.clone();
            let station = station.clone();
            children.spawn(async move {
                let Ok(Ok(mut peer)) = tokio::time::timeout(
                    HANDSHAKE_TIMEOUT,
                    handshake(stream, capability_path, protocol),
                )
                .await
                else {
                    return;
                };
                drop(permit);
                let mut request = upstream.into_client_request().unwrap();
                request
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
                let credentials = format!(
                    "{}:{STATION_SECRET}-{}",
                    station.station_id.as_str(),
                    station.station_id.as_str()
                );
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, credentials);
                request
                    .headers_mut()
                    .insert(AUTHORIZATION, format!("Basic {encoded}").parse().unwrap());
                let Ok((mut server, _)) = connect_async(request).await else {
                    return;
                };
                loop {
                    tokio::select! {
                        item = peer.next() => match item {
                            Some(Ok(Message::Text(frame))) => {
                                let parsed = serde_json::from_str::<Value>(&frame).ok();
                                if protocol == "ocpp1.6" && parsed.as_ref().is_some_and(|p| p[2] == "Authorize") {
                                    let result = uob_protocol_adapter::v16::authorize_call(
                                        frame.as_bytes(), &station, &auth,
                                        &uob_provider_adapter::LocalAuthorizationProvider,
                                        Clock.now()).await;
                                    let Ok(outcome) = result else { return };
                                    let Some(response) = outcome.authorize_response_frame() else { return };
                                    if peer.send(Message::Text(response.to_string().into())).await.is_err() { return; }
                                } else if server.send(Message::Text(frame)).await.is_err() { return; }
                            }
                            Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                            Some(Ok(other)) => { if server.send(other).await.is_err() { return; } }
                        },
                        item = server.next() => match item {
                            Some(Ok(message)) => { if peer.send(message).await.is_err() { return; } }
                            _ => return,
                        },
                    }
                }
            });
        }
    });
    (endpoint, task)
}
