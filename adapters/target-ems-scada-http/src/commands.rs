//! Authenticated HTTP mapping onto the supervised application's command lifecycle.

use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{FromRequest, Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::Semaphore;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, CommandAdmissionFuture, CommandAdmissionPort,
    ScopedCommandAdmissionPort, TargetQuery,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandOperation, CommandRequest, CommandResult, ExternalCommand,
    RequestId,
};

use crate::{
    configuration::IntegrationPrincipal, error::IntegrationErrorCode, reads::CanonicalRead,
    routing::IntegrationState,
};

mod response;
#[cfg(test)]
mod tests;

/// Erases only the host's privileged payload type; ordinary requests use canonical models.
pub(crate) trait IntegrationCommands: Send + Sync {
    fn submit(
        &self,
        payload: Value,
        grant: AccessGrant,
    ) -> CommandAdmissionFuture<'_, CommandResult>;
}

pub(crate) struct SupervisedCommands<P>(Arc<dyn CommandAdmissionPort<P>>);

impl<P> SupervisedCommands<P> {
    pub(crate) fn new(port: Arc<dyn CommandAdmissionPort<P>>) -> Self {
        Self(port)
    }
}

impl<P: DeserializeOwned + Send + 'static> IntegrationCommands for SupervisedCommands<P> {
    fn submit(
        &self,
        payload: Value,
        grant: AccessGrant,
    ) -> CommandAdmissionFuture<'_, CommandResult> {
        Box::pin(async move {
            let request: CommandRequest<P> = serde_json::from_value(payload).map_err(|_| {
                uob_application::CommandAdmissionError::new(
                    uob_application::CommandAdmissionErrorCode::PolicyRejected,
                    "ems_scada_http.invalid_request",
                )
            })?;
            let command = ExternalCommand::authenticated(request, grant.origin().clone());
            // The calling credential can narrow, but never widen, the host's authorization.
            ScopedCommandAdmissionPort::new(Arc::clone(&self.0), AccessPolicy::single(grant))
                .submit(command)
                .await
        })
    }
}

/// Admission is bounded independently of concurrent reads and by a finite request deadline.
pub(crate) struct CommandExecutor {
    commands: Arc<dyn IntegrationCommands>,
    deadline: Duration,
    in_flight: Semaphore,
}

impl CommandExecutor {
    pub(crate) fn new(
        commands: Arc<dyn IntegrationCommands>,
        deadline: Duration,
        maximum_in_flight: usize,
    ) -> Self {
        Self {
            commands,
            deadline,
            in_flight: Semaphore::new(maximum_in_flight),
        }
    }
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct AcceptedCommand {
    request_id: String,
    status_url: String,
    result: CommandResult,
}

pub(crate) async fn submit(State(state): State<IntegrationState>, request: Request) -> Response {
    let Ok(_permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    let principal = match state
        .authenticate(request.headers())
        .and_then(require_operator)
    {
        Ok(principal) => principal,
        Err(error) => return error.into_response(),
    };
    let Some(executor) = state.commands() else {
        return IntegrationErrorCode::SourceUnavailable.into_response();
    };
    let Ok(_command_permit) = executor.in_flight.try_acquire() else {
        return IntegrationErrorCode::CommandBusy.into_response();
    };
    // Include body reading in the deadline and acquire permits before reading or parsing JSON.
    match tokio::time::timeout(
        executor.deadline,
        submit_request(executor, principal, request),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => IntegrationErrorCode::DeadlineExceeded.into_response(),
    }
}

async fn submit_request(
    executor: &CommandExecutor,
    principal: &IntegrationPrincipal,
    request: Request,
) -> Response {
    let payload = match Json::<Value>::from_request(request, &()).await {
        Ok(Json(payload)) => payload,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return IntegrationErrorCode::PayloadTooLarge.into_response();
        }
        Err(_) => return IntegrationErrorCode::InvalidRequest.into_response(),
    };
    let Ok(command) = serde_json::from_value::<CommandRequest<Value>>(payload.clone()) else {
        return IntegrationErrorCode::InvalidRequest.into_response();
    };
    if !valid_request_id(&command.request_id) {
        return IntegrationErrorCode::InvalidRequest.into_response();
    }
    if !principal
        .grant()
        .permits(AccessPermission::Control, &command.resource)
        || matches!(command.operation, CommandOperation::Ocpp(_))
    {
        return IntegrationErrorCode::PermissionDenied.into_response();
    }
    match executor
        .commands
        .submit(payload, principal.grant().clone())
        .await
    {
        Ok(result) => {
            if result.return_route.request_id != command.request_id
                || result.return_route.origin != *principal.grant().origin()
                || result.resource != command.resource
            {
                return IntegrationErrorCode::SourceUnavailable.into_response();
            }
            response::command(result)
        }
        Err(error) => response::admission_error(error.code()).into_response(),
    }
}

pub(crate) async fn status(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    request_id: Result<Path<String>, axum::extract::rejection::PathRejection>,
) -> Response {
    let Ok(_permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    let principal = match state.authenticate(&headers).and_then(require_operator) {
        Ok(principal) => principal,
        Err(error) => return error.into_response(),
    };
    let Ok(Path(request_id)) = request_id else {
        return IntegrationErrorCode::InvalidRequest.into_response();
    };
    let Ok(request_id) = RequestId::new(request_id) else {
        return IntegrationErrorCode::InvalidRequest.into_response();
    };
    if !valid_request_id(&request_id) {
        return IntegrationErrorCode::InvalidRequest.into_response();
    }
    match state
        .reads()
        .read(TargetQuery::CommandResult(request_id.clone()))
        .await
    {
        Ok(CanonicalRead::CommandResult(result)) => match *result {
            Some(result) => {
                if !principal
                    .grant()
                    .permits(AccessPermission::Control, &result.resource)
                    || !same_target(&result.return_route.origin, principal.grant().origin())
                {
                    return IntegrationErrorCode::PermissionDenied.into_response();
                }
                if result.return_route.request_id != request_id {
                    return IntegrationErrorCode::SourceUnavailable.into_response();
                }
                Json(result).into_response()
            }
            None => IntegrationErrorCode::ResourceNotFound.into_response(),
        },
        Ok(_) => IntegrationErrorCode::SourceUnavailable.into_response(),
        Err(error) => error.into_response(),
    }
}

fn require_operator(
    principal: Option<&IntegrationPrincipal>,
) -> Result<&IntegrationPrincipal, IntegrationErrorCode> {
    principal
        .filter(|principal| principal.permissions().contains(&AccessPermission::Control))
        .ok_or(IntegrationErrorCode::PermissionDenied)
}

fn same_target(left: &AuthenticatedCommandOrigin, right: &AuthenticatedCommandOrigin) -> bool {
    matches!((left, right), (
        AuthenticatedCommandOrigin::Target { target_instance_id: left, .. },
        AuthenticatedCommandOrigin::Target { target_instance_id: right, .. }
    ) if left == right)
}

fn valid_request_id(request_id: &RequestId) -> bool {
    request_id.as_str().len() <= 256 && !matches!(request_id.as_str(), "." | "..")
}
