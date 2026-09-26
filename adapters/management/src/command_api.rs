use std::sync::Arc;

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Semaphore;
use uob_application::{
    CanonicalQuerySource, CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionPort,
    ScopedTargetQueryPort, TargetPortErrorCode, TargetQuery, TargetQueryAuthorization,
    TargetQueryPermission, TargetQueryPort, TargetQueryResult, TargetResourceScope,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CONFIGURATION_CHANGE_REFERENCE_SCHEMA, CommandLifecycle,
    CommandOperation, CommandRequest, ConfigurationChangeReference, ExternalCommand,
    PrivilegedOcppOperation,
};

use crate::{ManagementReadLimits, ManagementState};

mod reads;
pub(crate) use reads::{history, schemas, status};

/// Schema-aware validation boundary for privileged OCPP payloads.
pub trait PrivilegedPayloadValidator: Send + Sync {
    /// Validates the action, declared schema identity, and payload as one registry operation.
    ///
    /// # Errors
    ///
    /// Returns a stable sanitized code when the action/schema is unknown or the payload fails
    /// its pinned schema.
    fn validate(&self, operation: &PrivilegedOcppOperation<Value>) -> Result<(), &'static str>;
    /// Validates the addressed resource as well as the pinned payload when supported.
    ///
    /// # Errors
    ///
    /// Returns a stable sanitized code when the resource, action, schema, or payload is invalid.
    fn validate_resource(
        &self,
        resource: &uob_contracts::ResourceRef,
        operation: &PrivilegedOcppOperation<Value>,
    ) -> Result<(), &'static str> {
        let _ = resource;
        self.validate(operation)
    }
    /// Descriptors offered by a connected, explicitly capable station.
    fn schemas(&self, _snapshot: &uob_contracts::StationSnapshot) -> Vec<Value> {
        vec![]
    }
    /// Requires an exact descriptor match at HTTP admission before durable dispatch.
    fn offers(
        &self,
        _snapshot: &uob_contracts::StationSnapshot,
        _resource: &uob_contracts::ResourceRef,
        _operation: &PrivilegedOcppOperation<Value>,
    ) -> bool {
        false
    }
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
    /// Whether this authenticated principal may inspect control options for this station.
    fn permits_schema(
        &self,
        _origin: &AuthenticatedCommandOrigin,
        _resource: &uob_contracts::ResourceRef,
    ) -> bool {
        false
    }
    /// Protected, station-scoped opaque start reference; never raw charging identity.
    fn start_reference(
        &self,
        _origin: &AuthenticatedCommandOrigin,
        _resource: &uob_contracts::ResourceRef,
    ) -> Option<String> {
        None
    }
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
    source: Option<Arc<dyn CanonicalQuerySource<Value>>>,
    permits: Arc<Semaphore>,
    timeout: std::time::Duration,
}

impl ManagementCommands {
    pub(crate) fn new(
        configuration: ManagementCommandConfiguration,
        source: Option<Arc<dyn CanonicalQuerySource<Value>>>,
        limits: ManagementReadLimits,
    ) -> Self {
        Self {
            admission: configuration.admission,
            authenticator: configuration.authenticator,
            privileged_payloads: configuration.privileged_payloads,
            source,
            permits: Arc::new(Semaphore::new(limits.maximum_concurrent_queries)),
            timeout: limits.query_timeout,
        }
    }

    async fn station_snapshot(
        &self,
        station: uob_contracts::ResourceRef,
    ) -> Result<Option<uob_contracts::StationSnapshot>, Box<Response>> {
        let Some(source) = &self.source else {
            return Err(Box::new(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.schema_unavailable",
            )));
        };
        let _permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            Box::new(error(
                StatusCode::TOO_MANY_REQUESTS,
                "query.concurrency_limit",
            ))
        })?;
        let authorization = TargetQueryAuthorization::new(
            uob_contracts::TargetInstanceId::new("management-command-schema")
                .expect("static identity"),
            vec![TargetQueryPermission::StationSnapshots],
            vec![TargetResourceScope::Station {
                bridge_id: station.bridge_id.clone(),
                station_id: station.station_id.clone(),
            }],
        );
        let port = ScopedTargetQueryPort::new(source.clone(), authorization);
        match tokio::time::timeout(
            self.timeout,
            port.query(TargetQuery::StationSnapshot(station)),
        )
        .await
        {
            Ok(Ok(TargetQueryResult::StationSnapshot(snapshot))) => Ok(snapshot),
            Ok(Ok(_)) => Err(Box::new(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "command.response_type_mismatch",
            ))),
            Ok(Err(_)) => Err(Box::new(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.schema_unavailable",
            ))),
            Err(_) => Err(Box::new(error(
                StatusCode::GATEWAY_TIMEOUT,
                "query.deadline_exceeded",
            ))),
        }
    }

    async fn matching_command_result(
        &self,
        request: &CommandRequest<Value>,
        origin: &AuthenticatedCommandOrigin,
    ) -> Result<bool, Box<Response>> {
        let Some(source) = &self.source else {
            return Err(Box::new(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.schema_unavailable",
            )));
        };
        let _permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            Box::new(error(
                StatusCode::TOO_MANY_REQUESTS,
                "query.concurrency_limit",
            ))
        })?;
        let authorization = TargetQueryAuthorization::new(
            uob_contracts::TargetInstanceId::new("management-command-retry")
                .expect("static identity"),
            vec![TargetQueryPermission::CommandStatus],
            vec![TargetResourceScope::Resource(request.resource.clone())],
        );
        let port = ScopedTargetQueryPort::new(source.clone(), authorization);
        match tokio::time::timeout(
            self.timeout,
            port.query(TargetQuery::CommandResult(request.request_id.clone())),
        )
        .await
        {
            Ok(Ok(TargetQueryResult::CommandResult(Some(result)))) => {
                // The result cannot prove payload equality. Only the durable coordinator can
                // compare the complete authenticated command; this only skips transient offers.
                Ok(result.resource == request.resource && result.return_route.origin == *origin)
            }
            Ok(Ok(TargetQueryResult::CommandResult(None))) => Ok(false),
            Ok(Err(value)) if value.code() == TargetPortErrorCode::Unauthorized => Ok(false),
            Ok(Ok(_)) => Err(Box::new(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "command.response_type_mismatch",
            ))),
            Ok(Err(_)) => Err(Box::new(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.schema_unavailable",
            ))),
            Err(_) => Err(Box::new(error(
                StatusCode::GATEWAY_TIMEOUT,
                "query.deadline_exceeded",
            ))),
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
    if let Some(response) = privileged_schema_error(&commands, &origin, &request).await {
        return response;
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

async fn privileged_schema_error(
    commands: &ManagementCommands,
    origin: &AuthenticatedCommandOrigin,
    request: &CommandRequest<Value>,
) -> Option<Response> {
    let CommandOperation::Ocpp(operation) = &request.operation else {
        return None;
    };
    if let Err(code) = commands
        .privileged_payloads
        .validate_resource(&request.resource, operation)
    {
        return Some(error(StatusCode::BAD_REQUEST, code));
    }
    let station = uob_contracts::ResourceRef {
        bridge_id: request.resource.bridge_id.clone(),
        station_id: request.resource.station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    };
    if !commands.authenticator.permits_schema(origin, &station) {
        return Some(error(StatusCode::FORBIDDEN, "command.unauthorized"));
    }
    // A result scoped to this exact resource and authenticated origin is only a hint to
    // consult durable deduplication; the coordinator still checks the complete request and
    // scoped admission still authorizes the command.
    match commands.matching_command_result(request, origin).await {
        Ok(true) => return None,
        Ok(false) => {}
        Err(response) => return Some(*response),
    }

    let snapshot = match commands.station_snapshot(station).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            return Some(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "command.unsupported_schema",
            ));
        }
        Err(response) => return Some(*response),
    };
    if !commands
        .privileged_payloads
        .offers(&snapshot, &request.resource, operation)
    {
        return Some(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "command.unsupported_schema",
        ));
    }
    None
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
