//! Authenticated, bounded read bridge for the independent release supervisor.
use std::{path::PathBuf, sync::Arc, time::Duration};

use crate::release_read_protocol::{Code, EventsQuery, Request, Response};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response as HttpResponse},
    routing::get,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Semaphore,
    time::{Instant, timeout_at},
};

const DEADLINE: Duration = Duration::from_secs(1);
const MAX_REQUEST: usize = 1024;
const MAX_RESPONSE: usize = 80 * 1024;
const MAX_CONNECTIONS: usize = 4;

/// Authentication boundary for the separately provisioned release-read capability.
///
/// Implementations must compare the whole credential in constant time. The token is never sent to
/// the supervisor or application.
pub trait ManagementReleaseReadAuthenticator: Send + Sync {
    /// Returns true only for a release-read credential bound to this service environment.
    fn authenticate(&self, token: &str) -> bool;
}

/// Host-owned dependencies required to expose supervisor status and retained audit events.
#[derive(Clone)]
pub struct ManagementReleaseReadConfiguration {
    /// Fixed administrator-configured supervisor socket; no HTTP request can select a path.
    pub supervisor_socket: PathBuf,
    /// Independent read-only credential verifier.
    pub authenticator: Arc<dyn ManagementReleaseReadAuthenticator>,
}

#[derive(Clone)]
struct ReleaseReadState {
    supervisor_socket: Arc<PathBuf>,
    authenticator: Arc<dyn ManagementReleaseReadAuthenticator>,
    permits: Arc<Semaphore>,
}

impl From<ManagementReleaseReadConfiguration> for ReleaseReadState {
    fn from(value: ManagementReleaseReadConfiguration) -> Self {
        Self {
            supervisor_socket: Arc::new(value.supervisor_socket),
            authenticator: value.authenticator,
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }
}

/// Builds the independently authenticated supervisor read routes.
///
/// This exposes only `GET` status and cursor-bounded event reads. It has no release mutation
/// routes and never forwards a client-selected socket or IPC operation.
pub fn release_read_router(configuration: ManagementReleaseReadConfiguration) -> Router {
    Router::new()
        .route("/api/v1/release/status", get(status))
        .route("/api/v1/release/events", get(events))
        .with_state(ReleaseReadState::from(configuration))
}

async fn status(State(state): State<ReleaseReadState>, headers: HeaderMap) -> HttpResponse {
    let Ok(()) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    match request(&state, Request::Status {}).await {
        Ok(response) if response.status_result() => response_output(response),
        Ok(_) | Err(BridgeError::Protocol) => failure(StatusCode::BAD_GATEWAY, "release.protocol"),
        Err(BridgeError::Timeout) => failure(StatusCode::GATEWAY_TIMEOUT, "release.timeout"),
        Err(BridgeError::Unavailable) => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "release.unavailable")
        }
        Err(BridgeError::Busy) => {
            failure(StatusCode::TOO_MANY_REQUESTS, "release.connection_limit")
        }
    }
}

async fn events(
    State(state): State<ReleaseReadState>,
    headers: HeaderMap,
    query: Result<Query<EventsQuery>, axum::extract::rejection::QueryRejection>,
) -> HttpResponse {
    let Ok(()) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    let Ok(Query(query)) = query else {
        return failure(StatusCode::BAD_REQUEST, "release.invalid_cursor");
    };
    match request(&state, Request::Events { after: query.after }).await {
        Ok(response) if response.events_result(query.after) => response_output(response),
        Ok(_) | Err(BridgeError::Protocol) => failure(StatusCode::BAD_GATEWAY, "release.protocol"),
        Err(BridgeError::Timeout) => failure(StatusCode::GATEWAY_TIMEOUT, "release.timeout"),
        Err(BridgeError::Unavailable) => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "release.unavailable")
        }
        Err(BridgeError::Busy) => {
            failure(StatusCode::TOO_MANY_REQUESTS, "release.connection_limit")
        }
    }
}

fn authenticate(state: &ReleaseReadState, headers: &HeaderMap) -> Result<(), ()> {
    let header = headers
        .get(header::AUTHORIZATION)
        .ok_or(())?
        .to_str()
        .map_err(|_| ())?;
    let token = header
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
        .ok_or(())?;
    state
        .authenticator
        .authenticate(token)
        .then_some(())
        .ok_or(())
}

fn unauthenticated() -> HttpResponse {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(serde_json::json!({ "error": "release.unauthenticated" })),
    )
        .into_response()
}

fn failure(status: StatusCode, code: &'static str) -> HttpResponse {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

fn response_output(response: Response) -> HttpResponse {
    let status = match response.code {
        Code::Ok | Code::RecoveryRequired => StatusCode::OK,
        Code::Busy | Code::Forbidden => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(response)).into_response()
}

async fn request(state: &ReleaseReadState, request: Request) -> Result<Response, BridgeError> {
    let _permit = state
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| BridgeError::Busy)?;
    let mut encoded = serde_json::to_vec(&request).map_err(|_| BridgeError::Protocol)?;
    if encoded.len() > MAX_REQUEST {
        return Err(BridgeError::Protocol);
    }
    encoded.push(b'\n');
    let deadline = Instant::now() + DEADLINE;
    let mut stream = timeout_at(
        deadline,
        UnixStream::connect(state.supervisor_socket.as_ref()),
    )
    .await
    .map_err(|_| BridgeError::Timeout)?
    .map_err(|_| BridgeError::Unavailable)?;
    timeout_at(deadline, stream.write_all(&encoded))
        .await
        .map_err(|_| BridgeError::Timeout)?
        .map_err(|_| BridgeError::Unavailable)?;
    let encoded = read_response(&mut stream, deadline).await?;
    let response: Response = serde_json::from_slice(&encoded).map_err(|_| BridgeError::Protocol)?;
    response
        .valid()
        .then_some(response)
        .ok_or(BridgeError::Protocol)
}

async fn read_response(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, BridgeError> {
    timeout_at(deadline, async {
        let mut response = Vec::with_capacity(4096);
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|_| BridgeError::Unavailable)?;
            if read == 0 {
                return Err(BridgeError::Protocol);
            }
            if let Some(end) = chunk[..read].iter().position(|byte| *byte == b'\n') {
                if end + 1 != read || response.len() + end > MAX_RESPONSE {
                    return Err(BridgeError::Protocol);
                }
                response.extend_from_slice(&chunk[..end]);
                return Ok(response);
            }
            if response.len() + read > MAX_RESPONSE {
                return Err(BridgeError::Protocol);
            }
            response.extend_from_slice(&chunk[..read]);
        }
    })
    .await
    .map_err(|_| BridgeError::Timeout)?
}

enum BridgeError {
    Unavailable,
    Timeout,
    Protocol,
    Busy,
}
