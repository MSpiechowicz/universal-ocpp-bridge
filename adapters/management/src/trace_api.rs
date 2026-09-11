//! Best-effort, process-scoped trace SSE. The durable business-event API is independent.
mod subscriber;
#[cfg(test)]
mod tests;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::capture_api::{CaptureState, authenticate, failure, unauthenticated};
use uob_application::capture::CaptureError;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TraceQuery {
    after: Option<String>,
}

pub(crate) async fn traces(
    State(state): State<CaptureState>,
    Path((process, id)): Path<(String, u64)>,
    headers: HeaderMap,
    query: Result<Query<TraceQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(grant) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    let Ok(Query(query)) = query else {
        return failure(CaptureError::Invalid);
    };
    // Static cursor failures do not depend on a capture existing in this process.
    if process != state.identity.runtime.process_instance_id.as_str() {
        return gap_response("restart");
    }
    let after = match cursor(&headers, query.after.as_deref(), &process, id) {
        Ok(after) => after,
        Err("restart") => return gap_response("restart"),
        Err("expiry") => return gap_response("expiry"),
        Err(_) => return failure(CaptureError::Invalid),
    };
    // Authenticate the entire current selection before exposing any capture metadata.
    let status = match state.configuration.manager.status(&grant) {
        Ok(status) => status,
        Err(CaptureError::Gone) => return gap_response("expiry"),
        Err(error) => return failure(error),
    };
    if id != status.id {
        return gap_response("expiry");
    }
    let lease = match state.configuration.manager.lease(&grant, id, false) {
        Ok(lease) => lease,
        Err(CaptureError::Gone) => return gap_response("expiry"),
        Err(error) => return failure(error),
    };
    let initial = match lease.read_after(after) {
        Ok(value) => value,
        Err(CaptureError::Gone) => return gap_response("expiry"),
        Err(error) => return failure(error),
    };
    if after.is_some_and(|sequence| sequence >= initial.window.next_sequence) {
        return failure(CaptureError::Invalid);
    }
    subscriber::response(lease, process, id, after)
}

fn gap_response(reason: &'static str) -> Response {
    (
        StatusCode::GONE,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"code":"trace.gap", "reason":reason, "replay":"best_effort"})),
    )
        .into_response()
}

fn cursor(
    headers: &HeaderMap,
    query: Option<&str>,
    process: &str,
    id: u64,
) -> Result<Option<u64>, &'static str> {
    let mut values = headers.get_all("last-event-id").iter();
    let header = values
        .next()
        .map(|v| v.to_str())
        .transpose()
        .map_err(|_| "invalid")?;
    if values.next().is_some() || (header.is_some() && query.is_some()) {
        return Err("invalid");
    }
    let Some(value) = query.or(header).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 512 || value.chars().any(char::is_control) {
        return Err("invalid");
    }
    let (cursor_process, capture, sequence): (String, u64, u64) =
        serde_json::from_str(value).map_err(|_| "invalid")?;
    if cursor_process != process {
        return Err("restart");
    }
    if capture != id {
        return Err("expiry");
    }
    Ok(Some(sequence))
}
