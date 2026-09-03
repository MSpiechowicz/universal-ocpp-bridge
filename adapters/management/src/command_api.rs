use std::sync::Arc;

use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;
use uob_application::{
    Application, CanonicalQuerySource, CommandAdmissionError, CommandAdmissionErrorCode,
    CommandAdmissionPort, TargetQuery, TargetQueryAuthorization, TargetQueryResult,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandLifecycle, CommandOperation, CommandRequest,
    ExternalCommand, PrivilegedOcppOperation, RequestId,
};

use crate::{ManagementReadLimits, ManagementRouterOptions, ManagementState};

/// Builds the management router with scoped canonical reads and authenticated command access.
pub fn router_with_queries_and_commands(
    application: Application,
    source: Arc<dyn CanonicalQuerySource<Value>>,
    authorization: TargetQueryAuthorization,
    read_limits: ManagementReadLimits,
    commands: ManagementCommandConfiguration,
    options: ManagementRouterOptions,
) -> Router {
    crate::base_router(
        ManagementState {
            application,
            queries: Some(crate::read_api::ManagementQueries::new(
                source,
                authorization,
                read_limits,
            )),
            commands: Some(ManagementCommands::new(commands)),
            events: None,
        },
        options,
    )
}

/// Schema-aware validation boundary for privileged OCPP payloads.
pub trait PrivilegedPayloadValidator: Send + Sync {
    /// Validates the action, declared schema identity, and payload as one registry operation.
    ///
    /// # Errors
    ///
    /// Returns a stable sanitized code when the action/schema is unknown or the payload fails
    /// its pinned schema.
    fn validate(&self, operation: &PrivilegedOcppOperation<Value>) -> Result<(), &'static str>;
}

/// Authenticated command dependencies installed by the composition root.
pub struct ManagementCommandConfiguration {
    /// Common application admission path, normally already wrapped in scoped access policy.
    pub admission: Arc<dyn CommandAdmissionPort<Value>>,
    /// Trusted identity established by the HTTP authentication layer, never request JSON.
    pub origin: AuthenticatedCommandOrigin,
    /// Registry used to reject unknown or schema-invalid privileged protocol requests.
    pub privileged_payloads: Arc<dyn PrivilegedPayloadValidator>,
}

#[derive(Clone)]
pub(crate) struct ManagementCommands {
    admission: Arc<dyn CommandAdmissionPort<Value>>,
    origin: AuthenticatedCommandOrigin,
    privileged_payloads: Arc<dyn PrivilegedPayloadValidator>,
}

impl ManagementCommands {
    pub(crate) fn new(configuration: ManagementCommandConfiguration) -> Self {
        Self {
            admission: configuration.admission,
            origin: configuration.origin,
            privileged_payloads: configuration.privileged_payloads,
        }
    }
}

#[derive(Serialize)]
struct AcceptedCommand {
    request_id: String,
    status_url: String,
    result: uob_contracts::CommandResult,
}

pub(crate) async fn submit(
    State(state): State<ManagementState>,
    payload: Result<Json<CommandRequest<Value>>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = payload else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_request");
    };
    let Some(commands) = state.commands else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command.admission_unavailable",
        );
    };
    if request.resource.bridge_id != state.application.identity().bridge_id {
        return error(StatusCode::FORBIDDEN, "command.resource_unauthorized");
    }
    if let CommandOperation::Ocpp(operation) = &request.operation
        && let Err(code) = commands.privileged_payloads.validate(operation)
    {
        return error(StatusCode::BAD_REQUEST, code);
    }
    let request_id = request.request_id.as_str().to_owned();
    match commands
        .admission
        .submit(ExternalCommand::authenticated(request, commands.origin))
        .await
    {
        Ok(result) => match &result.lifecycle {
            CommandLifecycle::Rejected {
                error: command_error,
            } => {
                let status = match command_error.code {
                    uob_contracts::CommandErrorCode::Unauthorized => StatusCode::FORBIDDEN,
                    uob_contracts::CommandErrorCode::Expired => StatusCode::GONE,
                    uob_contracts::CommandErrorCode::UnsupportedOperation => {
                        StatusCode::UNPROCESSABLE_ENTITY
                    }
                    uob_contracts::CommandErrorCode::StationDisconnected => StatusCode::CONFLICT,
                    uob_contracts::CommandErrorCode::InvalidParameters
                    | uob_contracts::CommandErrorCode::PolicyRejected
                    | uob_contracts::CommandErrorCode::ProtocolRejected => StatusCode::BAD_REQUEST,
                };
                (status, Json(result)).into_response()
            }
            _ => (
                StatusCode::ACCEPTED,
                Json(AcceptedCommand {
                    status_url: format!("/api/v1/commands/{request_id}"),
                    request_id,
                    result,
                }),
            )
                .into_response(),
        },
        Err(error_value) => admission_error(&error_value),
    }
}

pub(crate) async fn status(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    let Ok(request_id) = RequestId::new(request_id) else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_request_id");
    };
    if state.queries.is_none() && state.events.is_none() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command.status_unavailable",
        );
    }
    match crate::read_api::execute_query(&state, &headers, TargetQuery::CommandResult(request_id))
        .await
    {
        Ok(TargetQueryResult::CommandResult(Some(result))) => Json(result).into_response(),
        Ok(TargetQueryResult::CommandResult(None)) => {
            error(StatusCode::NOT_FOUND, "command.not_found")
        }
        Ok(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "command.response_type_mismatch",
        ),
        Err(error_value) => error_value.into_response(),
    }
}

fn admission_error(value: &CommandAdmissionError) -> Response {
    let (status, code) = match value.code() {
        CommandAdmissionErrorCode::Unauthorized => (StatusCode::FORBIDDEN, "command.unauthorized"),
        CommandAdmissionErrorCode::Expired => (StatusCode::GONE, "command.expired"),
        CommandAdmissionErrorCode::Unsupported => {
            (StatusCode::UNPROCESSABLE_ENTITY, "command.unsupported")
        }
        CommandAdmissionErrorCode::PolicyRejected => {
            (StatusCode::BAD_REQUEST, "command.policy_rejected")
        }
        CommandAdmissionErrorCode::Busy => (StatusCode::TOO_MANY_REQUESTS, "command.busy"),
        CommandAdmissionErrorCode::StorageCapacityExhausted
        | CommandAdmissionErrorCode::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "command.persistence_unavailable",
        ),
        CommandAdmissionErrorCode::InvalidRequest => {
            (StatusCode::CONFLICT, "command.request_conflict")
        }
    };
    error(status, code)
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}
