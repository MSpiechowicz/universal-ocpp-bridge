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
    AuthenticatedCommandOrigin, CONFIGURATION_CHANGE_REFERENCE_SCHEMA, CommandLifecycle,
    CommandOperation, CommandRequest, ConfigurationChangeReference, ExternalCommand,
    PrivilegedOcppOperation, RequestId,
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
/// Enforces the protected configuration envelope while preserving an existing pinned registry
/// for all other privileged actions. Install around the host's existing validator.
pub struct ConfigurationPayloadValidator {
    other: Arc<dyn PrivilegedPayloadValidator>,
}

impl ConfigurationPayloadValidator {
    #[must_use]
    pub fn new(other: Arc<dyn PrivilegedPayloadValidator>) -> Self {
        Self { other }
    }
}

impl PrivilegedPayloadValidator for ConfigurationPayloadValidator {
    fn validate(&self, operation: &PrivilegedOcppOperation<Value>) -> Result<(), &'static str> {
        if operation.protocol != uob_contracts::ProtocolEdition::Ocpp16j {
            return self.other.validate(operation);
        }
        match operation.action.as_str() {
            "ChangeConfiguration" => {
                if operation.payload_schema.as_str() != CONFIGURATION_CHANGE_REFERENCE_SCHEMA {
                    return Err("command.invalid_configuration_schema");
                }
                let Some(fields) = operation.payload.as_object() else {
                    return Err("command.invalid_configuration_payload");
                };
                let valid = fields.len() == 2
                    && fields
                        .get("key")
                        .and_then(Value::as_str)
                        .zip(fields.get("valueReference").and_then(Value::as_str))
                        .is_some_and(|(key, reference)| {
                            ConfigurationChangeReference::valid_parts(key, reference)
                        });
                if valid {
                    Ok(())
                } else {
                    Err("command.invalid_configuration_payload")
                }
            }
            "GetConfiguration" => {
                if operation.payload_schema.as_str()
                    != "urn:OCPP:1.6:2019:12:GetConfigurationRequest"
                {
                    return Err("command.invalid_configuration_schema");
                }
                let Some(fields) = operation.payload.as_object() else {
                    return Err("command.invalid_configuration_payload");
                };
                if fields.keys().any(|field| field != "key") {
                    return Err("command.invalid_configuration_payload");
                }
                if let Some(keys) = fields.get("key") {
                    let Some(keys) = keys.as_array() else {
                        return Err("command.invalid_configuration_payload");
                    };
                    if keys.len() > 256
                        || keys
                            .iter()
                            .any(|key| key.as_str().is_none_or(|key| key.chars().count() > 50))
                    {
                        return Err("command.invalid_configuration_payload");
                    }
                }
                Ok(())
            }
            _ => self.other.validate(operation),
        }
    }
}

/// Host-owned verifier of a request's command bearer credential.
///
/// Resolve credential material outside safe configuration and compare secrets in constant time.
/// Return only a trusted immutable origin; the request body never supplies that identity.
pub trait ManagementCommandAuthenticator: Send + Sync {
    /// Authenticates one syntactically valid bearer token as a management origin.
    fn authenticate(&self, bearer_token: &str) -> Option<AuthenticatedCommandOrigin>;
}

/// Authenticated command dependencies installed by the composition root.
pub struct ManagementCommandConfiguration {
    /// Common application admission path, normally already wrapped in scoped access policy.
    pub admission: Arc<dyn CommandAdmissionPort<Value>>,
    /// Per-request bearer verifier, independent of any event/read credentials.
    pub authenticator: Arc<dyn ManagementCommandAuthenticator>,
    /// Registry used to reject unknown or schema-invalid privileged protocol requests.
    pub privileged_payloads: Arc<dyn PrivilegedPayloadValidator>,
}

#[derive(Clone)]
pub(crate) struct ManagementCommands {
    admission: Arc<dyn CommandAdmissionPort<Value>>,
    authenticator: Arc<dyn ManagementCommandAuthenticator>,
    privileged_payloads: Arc<dyn PrivilegedPayloadValidator>,
}

impl ManagementCommands {
    pub(crate) fn new(configuration: ManagementCommandConfiguration) -> Self {
        Self {
            admission: configuration.admission,
            authenticator: configuration.authenticator,
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
    headers: HeaderMap,
    payload: Result<Json<CommandRequest<Value>>, JsonRejection>,
) -> Response {
    let Some(commands) = state.commands else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command.admission_unavailable",
        );
    };
    let Ok(origin) = commands.authenticate(&headers) else {
        return authentication_error();
    };
    let Ok(Json(request)) = payload else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_request");
    };
    if request.resource.bridge_id != state.application.identity().bridge_id {
        return error(StatusCode::FORBIDDEN, "command.resource_unauthorized");
    }
    if let CommandOperation::Ocpp(operation) = &request.operation
        && let Err(code) = commands.privileged_payloads.validate(operation)
    {
        return error(StatusCode::BAD_REQUEST, code);
    }
    let trace = state.application.diagnostics().span(
        request.correlation_id.clone(),
        Some(request.resource.station_id.clone()),
        None,
    );
    let trace = trace.with_request(request.request_id.clone());
    let request_id = request.request_id.as_str().to_owned();
    match commands
        .admission
        .submit(ExternalCommand::authenticated(request, origin))
        .await
    {
        Ok(result) => {
            trace.emit(
                uob_application::FlowStage::ManagementDelivery,
                uob_application::FlowEvidence::LocallyExposed,
            );
            match &result.lifecycle {
                CommandLifecycle::Rejected {
                    error: command_error,
                } => {
                    let status = match command_error.code {
                        uob_contracts::CommandErrorCode::Unauthorized => StatusCode::FORBIDDEN,
                        uob_contracts::CommandErrorCode::Expired => StatusCode::GONE,
                        uob_contracts::CommandErrorCode::UnsupportedOperation => {
                            StatusCode::UNPROCESSABLE_ENTITY
                        }
                        uob_contracts::CommandErrorCode::StationDisconnected => {
                            StatusCode::CONFLICT
                        }
                        uob_contracts::CommandErrorCode::InvalidParameters
                        | uob_contracts::CommandErrorCode::PolicyRejected
                        | uob_contracts::CommandErrorCode::ProtocolRejected => {
                            StatusCode::BAD_REQUEST
                        }
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
            }
        }
        Err(error_value) => admission_error(&error_value),
    }
}

pub(crate) async fn status(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    if state.queries.is_none() && state.events.is_none() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command.status_unavailable",
        );
    }
    // Event-backed reads use their own authenticated read grant. Query-backed reads
    // require the command caller's credential as well as the host's query scope.
    let origin = if state.events.is_none() {
        let Some(commands) = &state.commands else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.status_unavailable",
            );
        };
        let Ok(origin) = commands.authenticate(&headers) else {
            return authentication_error();
        };
        Some(origin)
    } else {
        None
    };
    let Ok(request_id) = RequestId::new(request_id) else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_request_id");
    };
    match crate::read_api::execute_query(&state, &headers, TargetQuery::CommandResult(request_id))
        .await
    {
        Ok(TargetQueryResult::CommandResult(Some(result))) => {
            if origin
                .as_ref()
                .is_some_and(|origin| *origin != result.return_route.origin)
            {
                return error(StatusCode::FORBIDDEN, "command.unauthorized");
            }
            Json(result).into_response()
        }
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

impl ManagementCommands {
    fn authenticate(&self, headers: &HeaderMap) -> Result<AuthenticatedCommandOrigin, ()> {
        let token = crate::event_api::bearer_token(headers)?;
        match self.authenticator.authenticate(token) {
            Some(origin @ AuthenticatedCommandOrigin::Management { .. }) => Ok(origin),
            _ => Err(()),
        }
    }
}

fn authentication_error() -> Response {
    error(StatusCode::UNAUTHORIZED, "command.authentication_required")
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
