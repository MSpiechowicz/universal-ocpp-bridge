use super::{DeferredConfigurationCall, DeferredLocalAuthorizationCall16, RemoteControlSession};
use crate::{OutboundCall, PendingCall, SessionSubmitError};
use serde_json::Value;
use tokio::time::Instant;
use uob_contracts::{Command, CommandErrorCode, ResourceRef, UtcTimestamp};

impl RemoteControlSession {
    fn enqueue_call(
        &self,
        action: &str,
        call: OutboundCall,
        deadline: Instant,
        resource: &ResourceRef,
        expires_at: UtcTimestamp,
    ) -> Result<PendingCall, SessionSubmitError> {
        if action == "SendLocalList" {
            let request = serde_json::from_value(call.payload.clone())
                .map_err(|_| SessionSubmitError::InvalidRequest)?;
            let provider = self
                .local_authorization_updates
                .as_ref()
                .ok_or(SessionSubmitError::InvalidRequest)?;
            let deferred = DeferredLocalAuthorizationCall16 {
                provider: provider.clone(),
                clock: self.clock.clone(),
                resource: resource.clone(),
                request,
                limits: self.local_list_limits.clone(),
                active: self.local_list_active.clone(),
                expires_at,
            };
            return self
                .handle
                .try_local_authorization_call_before(call, deadline, deferred);
        }
        if action == "ChangeConfiguration" {
            let provider = self
                .configuration_values
                .as_ref()
                .expect("validated provider");
            let deferred = DeferredConfigurationCall::new(
                provider.clone(),
                self.clock.clone(),
                resource.clone(),
                call.payload["key"]
                    .as_str()
                    .expect("validated key")
                    .to_owned(),
                call.payload["valueReference"]
                    .as_str()
                    .expect("validated reference")
                    .to_owned(),
            );
            self.handle
                .try_configuration_call_before(call, deadline, deferred)
        } else {
            self.handle.try_call_before(call, deadline)
        }
    }

    pub(super) fn enqueue_prepared(
        &self,
        command: &Command<Value>,
        action: &str,
        payload: Value,
        now: uob_contracts::UtcTimestamp,
    ) -> Result<PendingCall, CommandErrorCode> {
        if now >= command.expires_at {
            return Err(CommandErrorCode::Expired);
        }
        let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
        // Bound monotonic arithmetic even for an extreme externally supplied UTC expiry.
        let deadline = Instant::now() + remaining.min(std::time::Duration::from_hours(24));
        let call = OutboundCall {
            message_id: command.request_id.as_str().to_owned(),
            action: uob_contracts::ProtocolActionName::new(action).expect("static action"),
            payload,
            correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                uob_contracts::CorrelationId::new(command.request_id.as_str())
                    .expect("request identity")
            }),
        };
        self.enqueue_call(
            action,
            call,
            deadline,
            &command.resource,
            command.expires_at,
        )
        .map_err(|error| match error {
            SessionSubmitError::Closed => CommandErrorCode::StationDisconnected,
            SessionSubmitError::InvalidRequest => CommandErrorCode::InvalidParameters,
            SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
                CommandErrorCode::PolicyRejected
            }
        })
    }
}
