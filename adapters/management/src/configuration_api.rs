//! Bridge-scoped target configuration routes, authenticated independently of reads and commands.
//!
//! Every route requires an explicit bridge-wide configuration permission from the management
//! access policy. Apply only persists the next-start section; it never touches the running target.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tokio::sync::Semaphore;
use uob_application::{AccessGrant, AccessPermission};
use uob_contracts::{AuthenticatedCommandOrigin, PrincipalId, ServiceIdentity};

mod views;
pub use views::*;

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT_REQUESTS: usize = 4;
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// Future returned by the host configuration port.
pub type ConfigurationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ConfigurationApiError>> + Send + 'a>>;

/// Sanitized host failures; none carries a rejected value, credential, or storage detail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationApiError {
    /// Configuration, registry, or durable backlog is temporarily unavailable.
    Unavailable,
    /// The persisted configuration cannot be read as-is; the stable code names why.
    Unreadable(&'static str),
    /// This service has no writable next-start location configured.
    ApplyUnavailable,
    /// The persisted section changed since the client read its digest.
    Conflict,
    /// The candidate failed validation; apply persisted nothing.
    Invalid(Box<TargetValidationReport>),
    /// A disposition was refused with a stable reason code.
    DispositionRejected(&'static str),
}

/// Secret verification boundary returning the immutable management access grant.
///
/// Implementations compare the whole token in constant time. The token never reaches the host
/// configuration port, responses, or diagnostics.
pub trait ManagementConfigurationAuthenticator: Send + Sync {
    /// Resolves one syntactically valid bearer token.
    fn authenticate(&self, token: &str) -> Option<AccessGrant>;
}

/// Host-owned target configuration operations.
pub trait ManagementConfigurationPort: Send + Sync {
    /// Lists installed registry kinds.
    fn catalog(&self) -> ConfigurationFuture<'_, TargetCatalogView>;
    /// Reads the running destination, next-start section, backlog, and dispositions.
    fn current(&self) -> ConfigurationFuture<'_, TargetConfigurationView>;
    /// Validates a candidate without persisting it.
    fn validate(
        &self,
        candidate: TargetConfigurationCandidate,
    ) -> ConfigurationFuture<'_, TargetValidationReport>;
    /// Persists a valid candidate as the next-start section.
    fn apply(
        &self,
        request: TargetApplyRequest,
        actor: PrincipalId,
    ) -> ConfigurationFuture<'_, TargetApplyOutcome>;
    /// Records an audited archive or discard authorization for one old destination.
    fn authorize_disposition(
        &self,
        request: TargetDispositionRequest,
        actor: PrincipalId,
    ) -> ConfigurationFuture<'_, TargetDispositionView>;
}

/// Dependencies installed by the composition root.
#[derive(Clone)]
pub struct ManagementConfigurationSetup {
    /// Host configuration operations.
    pub port: Arc<dyn ManagementConfigurationPort>,
    /// Dedicated configuration credential verifier.
    pub authenticator: Arc<dyn ManagementConfigurationAuthenticator>,
}

#[derive(Clone)]
struct ConfigurationState {
    setup: ManagementConfigurationSetup,
    identity: Arc<ServiceIdentity>,
    permits: Arc<Semaphore>,
}

/// Builds the independently authenticated target configuration routes.
pub fn configuration_router(
    identity: ServiceIdentity,
    setup: ManagementConfigurationSetup,
) -> Router {
    Router::new()
        .route("/api/v1/configuration/targets", get(current))
        .route("/api/v1/configuration/targets/catalog", get(catalog))
        .route("/api/v1/configuration/targets/validate", post(validate))
        .route("/api/v1/configuration/targets/apply", post(apply))
        .route(
            "/api/v1/configuration/targets/dispositions",
            post(authorize_disposition),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(ConfigurationState {
            setup,
            identity: Arc::new(identity),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        })
}

async fn catalog(State(state): State<ConfigurationState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers, &[AccessPermission::ConfigurationRead]) {
        return *response;
    }
    run(&state, state.setup.port.catalog(), StatusCode::OK).await
}

async fn current(State(state): State<ConfigurationState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers, &[AccessPermission::ConfigurationRead]) {
        return *response;
    }
    run(&state, state.setup.port.current(), StatusCode::OK).await
}

async fn validate(
    State(state): State<ConfigurationState>,
    headers: HeaderMap,
    payload: Result<Json<TargetConfigurationCandidate>, JsonRejection>,
) -> Response {
    if let Err(response) = authorize(&state, &headers, &[AccessPermission::ConfigurationRead]) {
        return *response;
    }
    let Ok(Json(candidate)) = payload else {
        return failure(StatusCode::BAD_REQUEST, "configuration.invalid_request");
    };
    run(&state, state.setup.port.validate(candidate), StatusCode::OK).await
}

async fn apply(
    State(state): State<ConfigurationState>,
    headers: HeaderMap,
    payload: Result<Json<TargetApplyRequest>, JsonRejection>,
) -> Response {
    let actor = match authorize(&state, &headers, &[AccessPermission::ConfigurationWrite]) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let Ok(Json(request)) = payload else {
        return failure(StatusCode::BAD_REQUEST, "configuration.invalid_request");
    };
    run(
        &state,
        state.setup.port.apply(request, actor),
        StatusCode::OK,
    )
    .await
}

async fn authorize_disposition(
    State(state): State<ConfigurationState>,
    headers: HeaderMap,
    payload: Result<Json<TargetDispositionRequest>, JsonRejection>,
) -> Response {
    let grant = match authenticate(&state, &headers) {
        Ok(grant) => grant,
        Err(response) => return *response,
    };
    let Ok(Json(request)) = payload else {
        return failure(StatusCode::BAD_REQUEST, "configuration.invalid_request");
    };
    let required: &[AccessPermission] = match request.action {
        DispositionAction::Archive => &[AccessPermission::ConfigurationWrite],
        DispositionAction::Discard => &[
            AccessPermission::ConfigurationWrite,
            AccessPermission::DestructiveDisposition,
        ],
    };
    let actor = match permitted(&state, &grant, required) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    run(
        &state,
        state.setup.port.authorize_disposition(request, actor),
        StatusCode::CREATED,
    )
    .await
}

fn authenticate(
    state: &ConfigurationState,
    headers: &HeaderMap,
) -> Result<AccessGrant, Box<Response>> {
    let token =
        crate::event_api::bearer_token(headers).map_err(|()| Box::new(unauthenticated()))?;
    state
        .setup
        .authenticator
        .authenticate(token)
        .ok_or_else(|| Box::new(unauthenticated()))
}

/// Authenticates the bearer token and requires every permission over this whole bridge.
fn authorize(
    state: &ConfigurationState,
    headers: &HeaderMap,
    required: &[AccessPermission],
) -> Result<PrincipalId, Box<Response>> {
    permitted(state, &authenticate(state, headers)?, required)
}

fn permitted(
    state: &ConfigurationState,
    grant: &AccessGrant,
    required: &[AccessPermission],
) -> Result<PrincipalId, Box<Response>> {
    let AuthenticatedCommandOrigin::Management { principal_id } = grant.origin() else {
        return Err(Box::new(forbidden()));
    };
    if required
        .iter()
        .all(|permission| grant.permits_bridge(*permission, &state.identity.bridge_id))
    {
        Ok(principal_id.clone())
    } else {
        Err(Box::new(forbidden()))
    }
}

async fn run<T: Serialize>(
    state: &ConfigurationState,
    operation: ConfigurationFuture<'_, T>,
    success: StatusCode,
) -> Response {
    let Ok(_permit) = state.permits.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "configuration.busy");
    };
    match tokio::time::timeout(REQUEST_DEADLINE, operation).await {
        Ok(Ok(value)) => no_store(success, Json(value)),
        Ok(Err(error)) => port_failure(error),
        Err(_) => failure(
            StatusCode::GATEWAY_TIMEOUT,
            "configuration.deadline_exceeded",
        ),
    }
}

fn port_failure(error: ConfigurationApiError) -> Response {
    match error {
        ConfigurationApiError::Unavailable => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "configuration.unavailable")
        }
        ConfigurationApiError::Unreadable(code) => failure(StatusCode::SERVICE_UNAVAILABLE, code),
        ConfigurationApiError::ApplyUnavailable => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "configuration.apply_unavailable",
        ),
        ConfigurationApiError::Conflict => failure(StatusCode::CONFLICT, "configuration.conflict"),
        ConfigurationApiError::Invalid(report) => no_store(
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "configuration.invalid", "validation": report })),
        ),
        ConfigurationApiError::DispositionRejected(code) => failure(StatusCode::CONFLICT, code),
    }
}

fn unauthenticated() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::WWW_AUTHENTICATE, "Bearer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(serde_json::json!({ "error": "configuration.unauthenticated" })),
    )
        .into_response()
}

fn forbidden() -> Response {
    failure(StatusCode::FORBIDDEN, "configuration.forbidden")
}

fn failure(status: StatusCode, code: &'static str) -> Response {
    no_store(status, Json(serde_json::json!({ "error": code })))
}

fn no_store(status: StatusCode, body: impl IntoResponse) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], body).into_response()
}
