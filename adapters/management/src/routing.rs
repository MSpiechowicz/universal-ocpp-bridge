use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};
use serde_json::Value;
use uob_application::{Application, CanonicalQuerySource, TargetQueryAuthorization};

use crate::{
    ManagementCommandConfiguration, ManagementEventConfiguration, ManagementReadLimits,
    ManagementRouterOptions, assets, command_api, event_api, read_api,
};

/// Request state for the target-independent management listener.
#[derive(Clone)]
pub(crate) struct ManagementState {
    pub(crate) application: Application,
    pub(crate) queries: Option<read_api::ManagementQueries>,
    pub(crate) commands: Option<command_api::ManagementCommands>,
    pub(crate) events: Option<event_api::ManagementEvents>,
}

/// Builds the management router while keeping framework types out of the application.
pub fn router(application: Application) -> Router {
    router_with_options(application, ManagementRouterOptions::default())
}

/// Builds the same API router with optional static browser assets.
pub fn router_with_options(application: Application, options: ManagementRouterOptions) -> Router {
    base_router(
        ManagementState {
            application,
            queries: None,
            commands: None,
            events: None,
        },
        options,
    )
}

/// Builds the management router with scoped canonical station reads.
pub fn router_with_queries(
    application: Application,
    source: Arc<dyn CanonicalQuerySource<Value>>,
    authorization: TargetQueryAuthorization,
    limits: ManagementReadLimits,
    options: ManagementRouterOptions,
) -> Router {
    base_router(
        ManagementState {
            application,
            queries: Some(read_api::ManagementQueries::new(
                source,
                authorization,
                limits,
            )),
            commands: None,
            events: None,
        },
        options,
    )
}

/// Builds canonical reads plus bearer-authenticated durable management events.
pub fn router_with_authenticated_events(
    application: Application,
    source: Arc<dyn CanonicalQuerySource<Value>>,
    read_limits: ManagementReadLimits,
    event_configuration: ManagementEventConfiguration,
    options: ManagementRouterOptions,
) -> Router {
    base_router(
        ManagementState {
            application,
            queries: None,
            commands: None,
            events: Some(event_api::ManagementEvents::new(
                source,
                event_configuration,
                read_limits,
            )),
        },
        options,
    )
}

/// Combines bearer-authenticated reads/events with the existing host-configured command handler.
///
/// Command authentication remains the independent boundary described by
/// [`ManagementCommandConfiguration`]; event bearer credentials are not reused for submissions.
pub fn router_with_commands_and_authenticated_events(
    application: Application,
    source: Arc<dyn CanonicalQuerySource<Value>>,
    read_limits: ManagementReadLimits,
    commands: ManagementCommandConfiguration,
    event_configuration: ManagementEventConfiguration,
    options: ManagementRouterOptions,
) -> Router {
    base_router(
        ManagementState {
            application,
            queries: None,
            commands: Some(command_api::ManagementCommands::new(commands)),
            events: Some(event_api::ManagementEvents::new(
                source,
                event_configuration,
                read_limits,
            )),
        },
        options,
    )
}

pub(crate) fn base_router(state: ManagementState, options: ManagementRouterOptions) -> Router {
    let router = Router::new()
        .route("/health", get(crate::health))
        .route("/api/v1/health", get(crate::detailed_health))
        .route("/metrics", get(crate::metrics))
        .route("/api/v1/identity", get(crate::identity))
        .route("/api/v1/stations", get(read_api::stations))
        .route("/api/v1/stations/{station_id}", get(read_api::station))
        .route("/api/v1/events", get(event_api::events))
        .route("/api/v1/commands", post(command_api::submit))
        .route("/api/v1/commands/{request_id}", get(command_api::status));
    let router = if options.static_assets {
        router.route("/", get(assets::browser_entry))
    } else {
        router
    };
    router.with_state(state)
}
