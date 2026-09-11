//! Finite, revocable JSONL exports; no snapshots, payload queues or server-side files.
mod format;
mod stream;
#[cfg(test)]
mod tests;

use crate::capture_api::{CaptureState, authenticate, failure, unauthenticated};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, header},
    response::Response,
};
use uob_application::capture::CaptureError;

pub(crate) async fn export(
    State(state): State<CaptureState>,
    Path((process, id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Response {
    let Ok(grant) = authenticate(&state, &headers) else {
        return unauthenticated();
    };
    if process != state.identity.runtime.process_instance_id.as_str() {
        return failure(CaptureError::Gone);
    }
    let status = match state.configuration.manager.status(&grant) {
        Ok(status) if status.id == id => status,
        Ok(_) => return failure(CaptureError::Gone),
        Err(error) => return failure(error),
    };
    let lease = match state.configuration.manager.lease(&grant, id, true) {
        Ok(lease) => lease,
        Err(error) => return failure(error),
    };
    let window = match lease.read_after(None) {
        Ok(read) => read.window,
        Err(error) => return failure(error),
    };
    let limits = stream::Limits::default();
    let Ok(manifest) = format::manifest(&state.identity, &status, window, limits) else {
        return failure(CaptureError::Capacity);
    };
    // Retain only the already validated, <=8 KiB credential. It is never serialized.
    let mut credential = HeaderMap::new();
    credential.insert(
        header::AUTHORIZATION,
        headers[header::AUTHORIZATION].clone(),
    );
    stream::response(
        state.configuration,
        credential,
        id,
        lease,
        window,
        manifest,
        limits,
    )
}
