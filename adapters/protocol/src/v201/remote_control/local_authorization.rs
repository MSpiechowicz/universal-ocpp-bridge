use super::{
    RemoteControlSession, local_authorization_schema,
    local_authorization_values::LocalAuthorizationUpdates201,
    local_authorization_wire::DeferredLocalAuthorizationCall201, mapping,
};
use crate::{
    OutboundCall, SessionCallOutcome, SessionSubmitError,
    command_registry::local_authorization201 as registry,
};
use serde_json::Value;
use std::sync::Arc;
use uob_application::CommandDispatchOutcome;
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, CorrelationId, LocalAuthorizationResult201,
    SendLocalListReference201,
};

impl RemoteControlSession {
    #[must_use]
    pub fn with_local_authorization_updates(
        mut self,
        provider: Arc<LocalAuthorizationUpdates201>,
    ) -> Self {
        self.local_authorization_updates = Some(provider);
        self
    }
    pub(super) async fn dispatch_local_authorization(
        &self,
        command: Command<Value>,
    ) -> CommandDispatchOutcome {
        let now = self.clock.now();
        if now >= command.expires_at {
            return mapping::not_sent(CommandErrorCode::Expired);
        }
        if self.handle.is_closed() {
            return mapping::not_sent(CommandErrorCode::StationDisconnected);
        }
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return mapping::not_sent(CommandErrorCode::InvalidParameters);
        };
        let request = match registry::validate(&command.resource, operation) {
            Ok(request) => request,
            Err(code) => return mapping::not_sent(code),
        };
        let action = operation.action.as_str();
        let index = registry::ACTIONS
            .iter()
            .position(|name| *name == action)
            .expect("validated action");
        let pending = {
            let Ok(snapshot) = self.snapshot.read() else {
                return mapping::not_sent(CommandErrorCode::PolicyRejected);
            };
            if !matches!(snapshot.connectivity, uob_contracts::Connectivity::Connected { connected_at, .. } if command.admitted_at >= connected_at)
                || uob_application::registration::v201::accepted(&snapshot).is_err()
            {
                return mapping::not_sent(CommandErrorCode::StationDisconnected);
            }
            if mapping::capabilities(&snapshot, &command.resource)
                .is_none_or(|caps| command.validate_for_dispatch(caps, now).is_err())
            {
                return mapping::not_sent(CommandErrorCode::UnsupportedOperation);
            }
            let limits = match self.configuration_limits.lock() {
                Ok(limits) => limits.local.clone(),
                Err(_) => return mapping::not_sent(CommandErrorCode::PolicyRejected),
            };
            let deferred = DeferredLocalAuthorizationCall201 {
                provider: self.local_authorization_updates.clone(),
                clock: self.clock.clone(),
                resource: command.resource.clone(),
                request,
                action: registry::ACTIONS[index],
                authority: self.local_authorization_authority[index].clone(),
                limits,
                active: self.configuration_active.clone(),
                expires_at: command.expires_at,
            };
            let call = OutboundCall {
                message_id: command.request_id.as_str().to_owned(),
                action: operation.action.clone(),
                payload: operation.payload.clone(),
                correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                    CorrelationId::new(command.request_id.as_str()).expect("request identity")
                }),
            };
            let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
            let deadline =
                tokio::time::Instant::now() + remaining.min(std::time::Duration::from_hours(24));
            match self
                .handle
                .try_local_authorization_201_call_before(call, deadline, deferred)
            {
                Ok(pending) => pending,
                Err(error) => {
                    return mapping::not_sent(match error {
                        SessionSubmitError::Closed => CommandErrorCode::StationDisconnected,
                        SessionSubmitError::InvalidRequest => CommandErrorCode::InvalidParameters,
                        SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
                            CommandErrorCode::PolicyRejected
                        }
                    });
                }
            }
        };
        let (outcome, _reservation) = pending.receive_guarded().await;
        match outcome {
            SessionCallOutcome::Result { mut payload, .. } => {
                let response = response(action, &payload, &command);
                super::local_authorization_values::wipe_local_authorization_json(&mut payload);
                response
            }
            SessionCallOutcome::Error { .. } => mapping::rejected_response(),
            SessionCallOutcome::NotTransmitted { .. } => {
                mapping::not_sent(if self.clock.now() >= command.expires_at {
                    CommandErrorCode::Expired
                } else {
                    CommandErrorCode::PolicyRejected
                })
            }
            SessionCallOutcome::TimedOut { .. }
            | SessionCallOutcome::TransmissionUncertain { .. } => mapping::uncertain(),
        }
    }
}

fn response(action: &str, payload: &Value, command: &Command<Value>) -> CommandDispatchOutcome {
    let index = match action {
        "GetLocalListVersion" => 1,
        "SendLocalList" => 2,
        "ClearCache" => 3,
        _ => return mapping::uncertain(),
    };
    if !local_authorization_schema::valid(index, payload) {
        return mapping::uncertain();
    }
    let evidence = match action {
        "GetLocalListVersion" => {
            let Some(version_number) = payload["versionNumber"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v >= 0)
            else {
                return mapping::uncertain();
            };
            LocalAuthorizationResult201::GetLocalListVersion { version_number }
        }
        "SendLocalList" => {
            let CommandOperation::Ocpp(operation) = &command.operation else {
                return mapping::uncertain();
            };
            let Ok(request) =
                serde_json::from_value::<SendLocalListReference201>(operation.payload.clone())
            else {
                return mapping::uncertain();
            };
            let Ok(status) = serde::Deserialize::deserialize(&payload["status"]) else {
                return mapping::uncertain();
            };
            LocalAuthorizationResult201::SendLocalList {
                version_number: request.version_number,
                update_type: request.update_type,
                status,
            }
        }
        "ClearCache" => {
            let Ok(status) = serde::Deserialize::deserialize(&payload["status"]) else {
                return mapping::uncertain();
            };
            LocalAuthorizationResult201::ClearCache { status }
        }
        _ => return mapping::uncertain(),
    };
    CommandDispatchOutcome::LocalAuthorizationResponse201(evidence)
}
