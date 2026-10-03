use super::{
    RemoteControlSession, configuration201_response,
    configuration201_values::LocalConfigurationValues201,
    configuration201_wire::DeferredConfigurationCall201, mapping,
};
use crate::{
    OutboundCall, SessionCallOutcome, SessionSubmitError,
    command_registry::configuration201 as registry,
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use uob_application::{CommandDispatchOutcome, WorkClass};
use uob_contracts::{
    CONFIGURATION_BYTES_LIMIT_201, Command, CommandErrorCode, CommandOperation, Connectivity,
    CorrelationId, UtcTimestamp,
};

impl RemoteControlSession {
    /// Installs immutable locally provisioned secrets without enabling any action or query.
    /// Learned limits are shared with explicit device-model reads on this exact socket generation.
    #[must_use]
    pub fn with_configuration_201(mut self, provider: Arc<LocalConfigurationValues201>) -> Self {
        self.configuration_201 = Some(provider);
        self
    }
    pub(super) async fn dispatch_configuration_201(
        &self,
        command: Command<Value>,
    ) -> CommandDispatchOutcome {
        let now = self.clock.now();
        if now >= command.expires_at {
            return mapping::not_sent(CommandErrorCode::Expired);
        }
        if self.handle.is_closed() || !self.configuration_active.lock().is_ok_and(|active| *active)
        {
            return mapping::not_sent(CommandErrorCode::StationDisconnected);
        }
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return mapping::not_sent(CommandErrorCode::InvalidParameters);
        };
        let request = match registry::validate(&command.resource, operation) {
            Ok(request) => Arc::new(request),
            Err(code) => return mapping::not_sent(code),
        };
        let Some(provider) = &self.configuration_201 else {
            return mapping::not_sent(CommandErrorCode::PolicyRejected);
        };
        if let Err(code) = self.validate_configuration_201_context(&command, now) {
            return mapping::not_sent(code);
        }
        let Some(metadata_size) = super::device_model_collection::json_size(
            &operation.payload,
            CONFIGURATION_BYTES_LIMIT_201,
        ) else {
            return mapping::not_sent(CommandErrorCode::InvalidParameters);
        };
        let Ok(metadata) = self
            .handle
            .resource_budget()
            .try_reserve(WorkClass::PendingRequest, metadata_size * 20 + 4096)
        else {
            return mapping::not_sent(CommandErrorCode::PolicyRejected);
        };
        let metadata = Arc::new(metadata);
        let CommandOperation::Ocpp(operation) = command.operation else {
            unreachable!("validated native operation");
        };
        let deferred = DeferredConfigurationCall201 {
            provider: provider.clone(),
            clock: self.clock.clone(),
            resource: command.resource.clone(),
            request: request.clone(),
            limits: self.configuration_limits.clone(),
            active: self.configuration_active.clone(),
            expires_at: command.expires_at,
            _metadata: metadata.clone(),
        };
        let call = OutboundCall {
            message_id: command.request_id.as_str().to_owned(),
            action: operation.action,
            payload: operation.payload,
            correlation_id: command.correlation_id.unwrap_or_else(|| {
                CorrelationId::new(command.request_id.as_str()).expect("request identity")
            }),
        };
        let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
        let deadline = Instant::now() + remaining.min(Duration::from_hours(24));
        let pending = match self
            .handle
            .try_configuration_201_call_before(call, deadline, deferred)
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
        };
        let (outcome, _response_reservation) = pending.receive_guarded().await;
        match outcome {
            SessionCallOutcome::Result { mut payload, .. } => {
                let response = configuration201_response::response(
                    request.as_ref(),
                    &payload,
                    &command.resource,
                );
                super::configuration201_values::wipe_json(&mut payload);
                response.map_or_else(
                    mapping::uncertain,
                    CommandDispatchOutcome::ConfigurationResponse201,
                )
            }
            SessionCallOutcome::Error { .. } => mapping::rejected_response(),
            SessionCallOutcome::NotTransmitted { reason, .. } => {
                mapping::not_sent(if reason == "command expired before socket send" {
                    CommandErrorCode::Expired
                } else {
                    CommandErrorCode::PolicyRejected
                })
            }
            SessionCallOutcome::TimedOut { .. }
            | SessionCallOutcome::TransmissionUncertain { .. } => mapping::uncertain(),
        }
    }

    fn validate_configuration_201_context(
        &self,
        command: &Command<Value>,
        now: UtcTimestamp,
    ) -> Result<(), CommandErrorCode> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        if !matches!(snapshot.connectivity, Connectivity::Connected { connected_at, .. } if command.admitted_at >= connected_at)
            || uob_application::registration::v201::accepted(&snapshot).is_err()
        {
            return Err(CommandErrorCode::StationDisconnected);
        }
        if mapping::capabilities(&snapshot, &command.resource)
            .is_none_or(|caps| command.validate_for_dispatch(caps, now).is_err())
        {
            return Err(CommandErrorCode::UnsupportedOperation);
        }
        Ok(())
    }
}
