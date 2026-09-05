use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::sync::{Semaphore, SemaphorePermit, TryAcquireError};
use uob_application::{PageLimit, TargetDescriptor};

use crate::{
    capabilities::{CapabilityDocument, ListenerLimits},
    commands,
    configuration::{IntegrationCredentials, IntegrationPrincipal},
    error::IntegrationErrorCode,
    points, reads, stations,
};

#[cfg(test)]
mod tests;

const MAX_AUTHORIZATION_BYTES: usize = 512;

/// Everything one running integration listener serves.
#[derive(Clone)]
pub(crate) struct IntegrationState {
    inner: Arc<IntegrationInner>,
}

struct IntegrationInner {
    descriptor: TargetDescriptor,
    limits: ListenerLimits,
    credentials: IntegrationCredentials,
    reads: reads::ReadExecutor,
    commands: Option<commands::CommandExecutor>,
    events: Option<crate::events::EventService>,
    in_flight: Semaphore,
}

impl IntegrationState {
    pub(crate) fn new(
        descriptor: TargetDescriptor,
        limits: ListenerLimits,
        credentials: IntegrationCredentials,
        reads: reads::ReadExecutor,
    ) -> Self {
        Self {
            inner: Arc::new(IntegrationInner {
                descriptor,
                in_flight: Semaphore::new(limits.maximum_concurrent_requests),
                limits,
                credentials,
                reads,
                commands: None,
                events: None,
            }),
        }
    }

    pub(crate) fn with_commands(mut self, commands: commands::CommandExecutor) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("unshared listener state")
            .commands = Some(commands);
        self
    }

    pub(crate) fn with_events(mut self, events: crate::events::EventService) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("unshared listener state")
            .events = Some(events);
        self
    }

    pub(crate) fn events(&self) -> Option<&crate::events::EventService> {
        self.inner.events.as_ref()
    }

    pub(crate) fn commands(&self) -> Option<&commands::CommandExecutor> {
        self.inner.commands.as_ref()
    }

    /// Reserves one of the listener's bounded concurrent-request slots.
    pub(crate) fn acquire(&self) -> Result<SemaphorePermit<'_>, TryAcquireError> {
        self.inner.in_flight.try_acquire()
    }

    /// Returns the deadline-bounded canonical read surface.
    pub(crate) fn reads(&self) -> &reads::ReadExecutor {
        &self.inner.reads
    }

    /// Returns how many station snapshots one point page may inspect.
    pub(crate) fn station_scan_limit(&self) -> PageLimit {
        self.inner.limits.station_scan_limit
    }

    /// Authenticates one request against the configured integration principals.
    ///
    /// A loopback listener with no configured credential file stays open to its local operator,
    /// mirroring the management API default; every other deployment requires a bearer token.
    pub(crate) fn authenticate(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<&IntegrationPrincipal>, IntegrationErrorCode> {
        if self.inner.credentials.is_empty() {
            return Ok(None);
        }
        let token = bearer_token(headers).ok_or(IntegrationErrorCode::Unauthenticated)?;
        self.inner
            .credentials
            .authenticate(token)
            .map(Some)
            .ok_or(IntegrationErrorCode::InvalidCredential)
    }
}

/// Builds the integration router.
///
/// Only `/bridge/v1` routes are mounted. There is no management, debug, capture, simulator, or
/// static-asset route to reach, and no outbound client is constructed.
pub(crate) fn integration_router(state: IntegrationState) -> Router {
    let body_limit = state.inner.limits.maximum_request_bytes;
    Router::new()
        .route("/bridge/v1/capabilities", get(capabilities))
        .route("/bridge/v1/events", get(crate::events::events))
        .route("/bridge/v1/stations", get(stations::stations))
        .route("/bridge/v1/stations/{station_id}", get(stations::station))
        .route("/bridge/v1/points", get(points::points))
        .route("/bridge/v1/points/{point_id}", get(points::point))
        .route("/bridge/v1/commands", post(commands::submit))
        .route("/bridge/v1/commands/{request_id}", get(commands::status))
        .fallback(unknown_resource)
        .method_not_allowed_fallback(unsupported_operation)
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

async fn capabilities(State(state): State<IntegrationState>, headers: HeaderMap) -> Response {
    let Ok(_permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    match state.authenticate(&headers) {
        Ok(principal) => Json(CapabilityDocument::new(
            &state.inner.descriptor,
            state.inner.limits,
            principal,
        ))
        .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn unknown_resource() -> Response {
    IntegrationErrorCode::UnknownResource.into_response()
}

async fn unsupported_operation() -> Response {
    IntegrationErrorCode::UnsupportedOperation.into_response()
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.as_bytes().len() > MAX_AUTHORIZATION_BYTES {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(token)
}
