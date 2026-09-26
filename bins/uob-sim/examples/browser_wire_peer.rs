//! Loopback-only OCPP peer for the disposable Playwright simulator fixture.
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header::SEC_WEBSOCKET_PROTOCOL},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const BIND: &str = "127.0.0.1:39195";
const STATIONS: [(&str, &str); 2] = [("demo-alpha", "ocpp1.6"), ("demo-beta", "ocpp2.0.1")];

struct PeerState {
    connections: u64,
    disconnections: u64,
    heartbeats: u64,
    replies: u64,
    remote_requests: u64,
    remote_replies: u64,
    last_remote_reply: Option<RemoteReply>,
    active: Option<mpsc::Sender<()>>,
    pending_request: bool,
}

#[derive(serde::Serialize)]
struct RemoteReply {
    elapsed_ms: u64,
    status: &'static str,
    request_id: String,
}

struct Station {
    id: &'static str,
    protocol: &'static str,
    state: Mutex<PeerState>,
}

impl Station {
    fn new(id: &'static str, protocol: &'static str) -> Self {
        Self {
            id,
            protocol,
            state: Mutex::new(PeerState {
                connections: 0,
                disconnections: 0,
                heartbeats: 0,
                replies: 0,
                remote_requests: 0,
                remote_replies: 0,
                last_remote_reply: None,
                active: None,
                pending_request: false,
            }),
        }
    }
}

#[derive(Clone)]
struct AppState(Arc<[Station; 2]>);

impl AppState {
    fn station(&self, id: &str) -> Option<&Station> {
        self.0.iter().find(|station| station.id == id)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let stations = STATIONS.map(|(id, protocol)| Station::new(id, protocol));
    let state = AppState(Arc::new(stations));
    let router = Router::new()
        .route("/observations", get(observations))
        .route("/commands/{station_id}/start", post(remote_start))
        .route("/{station_id}", get(websocket))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(BIND)
        .await
        .expect("loopback OCPP peer port unavailable");
    axum::serve(listener, router)
        .await
        .expect("OCPP peer failed");
}

async fn observations(State(state): State<AppState>) -> Json<Value> {
    let stations: Vec<_> = state
        .0
        .iter()
        .map(|station| {
            let observed = station.state.lock().expect("peer state poisoned");
            json!({
                "station_id": station.id,
                "protocol": station.protocol,
                "connections": observed.connections,
                "disconnections": observed.disconnections,
                "heartbeats": observed.heartbeats,
                "replies": observed.replies,
                "remote_requests": observed.remote_requests,
                "remote_replies": observed.remote_replies,
                "last_remote_reply": observed.last_remote_reply,
            })
        })
        .collect();
    Json(json!({ "stations": stations }))
}

async fn remote_start(Path(station_id): Path<String>, State(state): State<AppState>) -> StatusCode {
    let Some(station) = state.station(&station_id) else {
        return StatusCode::NOT_FOUND;
    };
    let mut observed = station.state.lock().expect("peer state poisoned");
    if observed.pending_request {
        return StatusCode::CONFLICT;
    }
    let Some(sender) = &observed.active else {
        return StatusCode::CONFLICT;
    };
    if sender.try_send(()).is_err() {
        return StatusCode::CONFLICT;
    }
    observed.pending_request = true;
    StatusCode::ACCEPTED
}

async fn websocket(
    Path(station_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let Some(station_index) = state.0.iter().position(|station| station.id == station_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let protocol = state.0[station_index].protocol;
    let offered = headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|header| header.split(','))
        .any(|offered| offered.trim() == protocol);
    if !offered {
        return StatusCode::BAD_REQUEST.into_response();
    }
    upgrade
        .protocols([protocol])
        .on_upgrade(move |socket| async move {
            serve_socket(socket, &state.0[station_index]).await;
        })
        .into_response()
}

async fn serve_socket(mut socket: WebSocket, station: &Station) {
    let (sender, mut commands) = mpsc::channel(1);
    let connection = {
        let mut state = station.state.lock().expect("peer state poisoned");
        state.connections += 1;
        state.active = Some(sender);
        state.connections
    };
    let mut pending: Option<(String, Instant)> = None;
    loop {
        tokio::select! {
            command = commands.recv() => {
                if command.is_none() {
                    break;
                }
                let (action, payload) = if station.protocol == "ocpp1.6" {
                    ("RemoteStartTransaction", json!({"idTag": "DEMO"}))
                } else {
                    ("RequestStartTransaction", json!({
                        "remoteStartId": 1,
                        "idToken": {"idToken": "DEMO", "type": "Central"}
                    }))
                };
                let request_id = {
                    let mut state = station.state.lock().expect("peer state poisoned");
                    state.remote_requests += 1;
                    format!("fixture-remote-{}", state.remote_requests)
                };
                let frame = json!([2, request_id, action, payload]);
                let sent = Instant::now();
                if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                    break;
                }
                pending = Some((request_id, sent));
            }
            frame = socket.recv() => {
                let Some(Ok(Message::Text(text))) = frame else {
                    break;
                };
                let Ok(call) = serde_json::from_str::<Value>(&text) else {
                    break;
                };
                if call[0] == 2 && call[2] == "Heartbeat" {
                    let response = json!([3, call[1], {"currentTime":"2026-09-01T00:00:00Z"}]);
                    station.state.lock().expect("peer state poisoned").heartbeats += 1;
                    if socket.send(Message::Text(response.to_string().into())).await.is_err() { break; }
                    station.state.lock().expect("peer state poisoned").replies += 1;
                } else if pending.as_ref().is_some_and(|(id, _)| call[0] == 3 && call[1] == id.as_str()) {
                    let (request_id, sent) = pending.take().expect("matching pending request");
                    let status = match call[2]["status"].as_str() {
                        Some("Accepted") => "Accepted",
                        Some("Rejected") => "Rejected",
                        _ => "invalid_reply",
                    };
                    let elapsed_ms = u64::try_from(sent.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let mut state = station.state.lock().expect("peer state poisoned");
                    state.remote_replies += 1;
                    state.last_remote_reply = Some(RemoteReply { elapsed_ms, status, request_id });
                    state.pending_request = false;
                }
            }
        }
    }
    let mut state = station.state.lock().expect("peer state poisoned");
    state.disconnections += 1;
    if state.connections == connection {
        state.active = None;
        state.pending_request = false;
    }
}
