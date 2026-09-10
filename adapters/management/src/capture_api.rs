//! Explicit diagnostic controls, independently authenticated from ordinary reads and commands.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::capture::{
    CaptureError, CaptureFilter, CaptureGrant, CaptureLevel, CaptureManager, CaptureStatus,
};
use uob_contracts::{ServiceIdentity, StationId, TargetInstanceId};

/// Secret verification boundary; implementations compare secrets in constant time and return
/// immutable diagnostic permissions/scopes. Tokens must never reach application diagnostics.
pub trait ManagementCaptureAuthenticator: Send + Sync {
    /// Resolves a bearer token to a trusted diagnostic grant.
    fn authenticate(&self, token: &str) -> Option<CaptureGrant>;
}

/// One service's shared capture authority and its credential verifier.
#[derive(Clone)]
pub struct ManagementCaptureConfiguration {
    /// Shared across every diagnostic route and eventual trace sink.
    pub manager: CaptureManager,
    /// Dedicated read/capture permission verification.
    pub authenticator: Arc<dyn ManagementCaptureAuthenticator>,
}

#[derive(Clone)]
struct CaptureState {
    configuration: ManagementCaptureConfiguration,
    identity: ServiceIdentity,
}

/// Builds independently authenticated control routes, mergeable with any management router.
/// Reading identity/health or mounting these routes never starts capture.
pub fn capture_router(
    identity: ServiceIdentity,
    configuration: ManagementCaptureConfiguration,
) -> Router {
    Router::new()
        .route("/api/v1/diagnostics/capture", get(status).post(start))
        .route(
            "/api/v1/diagnostics/capture/{process_id}/{id}/extend",
            post(extend),
        )
        .route(
            "/api/v1/diagnostics/capture/{process_id}/{id}/stop",
            post(stop),
        )
        .layer(DefaultBodyLimit::max(4096))
        .with_state(CaptureState {
            configuration,
            identity,
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    station_id: Option<StationId>,
    target_id: Option<TargetInstanceId>,
    #[serde(default)]
    level: Level,
    duration_seconds: Option<u64>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Level {
    #[default]
    Metadata,
    RedactedPayload,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Extension {
    duration_seconds: u64,
}

fn authenticate(state: &CaptureState, headers: &HeaderMap) -> Result<CaptureGrant, ()> {
    let values = headers.get_all(header::AUTHORIZATION);
    let mut values = values.iter();
    let token = values
        .next()
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 8192)
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| {
            !v.is_empty()
                && !v
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        });
    if values.next().is_some() {
        return Err(());
    }
    token
        .and_then(|token| state.configuration.authenticator.authenticate(token))
        .ok_or(())
}
fn unauthenticated() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"code":"capture.unauthenticated"})),
    )
        .into_response()
}
fn failure(error: CaptureError) -> Response {
    let (status, code) = match error {
        CaptureError::Disabled => (StatusCode::FORBIDDEN, "capture.disabled"),
        CaptureError::Forbidden => (StatusCode::FORBIDDEN, "capture.forbidden"),
        CaptureError::Conflict => (StatusCode::CONFLICT, "capture.conflict"),
        CaptureError::Invalid => (StatusCode::BAD_REQUEST, "capture.invalid"),
        CaptureError::Gone => (StatusCode::GONE, "capture.gone"),
        CaptureError::Capacity => (StatusCode::TOO_MANY_REQUESTS, "capture.capacity"),
    };
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"code":code})),
    )
        .into_response()
}
fn view(state: &CaptureState, status: &CaptureStatus) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(json!({
        "identity": state.identity, "id": status.id, "station_id": status.filter.station,
        "target_id": status.filter.target,
        "level": if status.level == CaptureLevel::Metadata { "metadata" } else { "redacted_payload" },
        "remaining_seconds": status.remaining.as_secs()
    }))).into_response()
}
async fn status(State(state): State<CaptureState>, headers: HeaderMap) -> Response {
    let Ok(grant) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    match state.configuration.manager.status(&grant) {
        Ok(status) => view(&state, &status),
        Err(e) => failure(e),
    }
}
async fn start(
    State(state): State<CaptureState>,
    headers: HeaderMap,
    payload: Result<Json<Start>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(grant) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    let Ok(Json(request)) = payload else {
        return failure(CaptureError::Invalid);
    };
    let filter = CaptureFilter {
        bridge: state.identity.bridge_id.clone(),
        station: request.station_id,
        target: request.target_id,
    };
    let level = match request.level {
        Level::Metadata => CaptureLevel::Metadata,
        Level::RedactedPayload => CaptureLevel::RedactedPayload,
    };
    match state.configuration.manager.start(
        &grant,
        filter,
        level,
        request.duration_seconds.map(Duration::from_secs),
    ) {
        Ok(status) => (StatusCode::CREATED, view(&state, &status)).into_response(),
        Err(e) => failure(e),
    }
}
async fn extend(
    State(state): State<CaptureState>,
    headers: HeaderMap,
    Path((process_id, id)): Path<(String, u64)>,
    payload: Result<Json<Extension>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(grant) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    let Ok(Json(request)) = payload else {
        return failure(CaptureError::Invalid);
    };
    if process_id != state.identity.runtime.process_instance_id.as_str() {
        return failure(CaptureError::Gone);
    }
    match state.configuration.manager.extend(
        &grant,
        id,
        Duration::from_secs(request.duration_seconds),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failure(e),
    }
}
async fn stop(
    State(state): State<CaptureState>,
    headers: HeaderMap,
    Path((process_id, id)): Path<(String, u64)>,
) -> Response {
    let Ok(grant) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    if process_id != state.identity.runtime.process_instance_id.as_str() {
        return failure(CaptureError::Gone);
    }
    match state.configuration.manager.stop(&grant, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failure(e),
    }
}
