//! Privileged OCPP 2.0.1 monitoring commands (N02 to N06). Every command is validated against the
//! pinned schema and the station's learned message limits before anything is transmitted; the
//! native reply is accepted as evidence only when it corresponds to the exact request.
use super::{
    RemoteControlSession, device_model_collection::json_size, mapping, monitoring_reply as reply,
};
use crate::command_registry::monitoring201 as registry;
use crate::{OutboundCall, PendingCall, SessionCallOutcome, SessionSubmitError};
use serde_json::Value;
use std::sync::Arc;
use tokio::time::Instant;
use uob_application::{CommandDispatchOutcome, MonitoringReportStore201};
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, Connectivity, CorrelationId, MonitoringQuery201,
    Operation, ProtocolEdition, StationSnapshot,
};

/// The OCPP-J frame cap a single CALL may reach.
const FRAME_BYTES: usize = 256 * 1024;

/// One validated command, ready to enqueue.
pub(super) struct Prepared {
    pub query: MonitoringQuery201,
    pub call: OutboundCall,
    pub deadline: Instant,
}

pub(super) fn owns(command: &Command<Value>) -> bool {
    matches!(&command.operation, CommandOperation::Ocpp(operation) if registry::owns(operation))
}

fn advertised(snapshot: &StationSnapshot) -> bool {
    let operation = Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp201,
        action: "GetMonitoringReport".to_owned(),
    };

    snapshot.capabilities.supports(&operation)
}

pub(super) fn submit_error(error: SessionSubmitError) -> CommandErrorCode {
    match error {
        SessionSubmitError::Closed => CommandErrorCode::StationDisconnected,
        SessionSubmitError::InvalidRequest => CommandErrorCode::InvalidParameters,
        SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
            CommandErrorCode::PolicyRejected
        }
    }
}

impl RemoteControlSession {
    /// Host binds durable report completion; without it `GetMonitoringReport` stays unsupported
    /// and an unsolicited `NotifyMonitoringReport` is answered `NotImplemented`. The other four
    /// actions need no store. Report collection ends with `Disconnected` when the host calls
    /// `detach_device_model` for a replaced or torn-down session.
    #[must_use]
    pub fn with_monitoring_reports(mut self, store: Arc<dyn MonitoringReportStore201>) -> Self {
        if self
            .snapshot
            .read()
            .is_ok_and(|snapshot| advertised(&snapshot))
        {
            self.handle.enable_monitoring_reports();
            self.monitoring_reports = Some(store);
        }

        self
    }

    pub(super) async fn dispatch_monitoring(
        &self,
        command: Command<Value>,
    ) -> CommandDispatchOutcome {
        let prepared = match self.prepare_monitoring(&command) {
            Ok(prepared) => prepared,
            Err(code) => return mapping::not_sent(code),
        };
        if matches!(
            prepared.query,
            MonitoringQuery201::GetMonitoringReport { .. }
        ) {
            return self.dispatch_monitoring_report(&command, prepared).await;
        }

        let Prepared {
            query,
            call,
            deadline,
        } = prepared;
        let pending = match self.handle.try_call_before(call, deadline) {
            Ok(pending) => pending,
            Err(error) => return mapping::not_sent(submit_error(error)),
        };

        receive(&query, pending).await
    }

    /// Validation, dispatch authority and message limits, in the order the charger would refuse.
    pub(super) fn prepare_monitoring(
        &self,
        command: &Command<Value>,
    ) -> Result<Prepared, CommandErrorCode> {
        use CommandErrorCode::{Expired, InvalidParameters, PolicyRejected, StationDisconnected};

        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Err(InvalidParameters);
        };
        let query = registry::validate(&command.resource, operation)?;
        let now = self.clock.now();
        {
            let snapshot = self.snapshot.read().map_err(|_| PolicyRejected)?;
            if self.handle.is_closed()
                || !matches!(snapshot.connectivity, Connectivity::Connected { connected_at, .. }
                    if command.admitted_at >= connected_at)
            {
                return Err(StationDisconnected);
            }
            uob_application::registration::v201::accepted(&snapshot).map_err(|_| PolicyRejected)?;
            let capabilities =
                mapping::capabilities(&snapshot, &command.resource).ok_or(InvalidParameters)?;
            command
                .validate_for_dispatch(capabilities, now)
                .map_err(|error| match error {
                    uob_contracts::CommandValidationError::Expired => Expired,
                    uob_contracts::CommandValidationError::UnsupportedOperation(_) => {
                        PolicyRejected
                    }
                })?;
        }
        if !self.within_limits(command, &query, operation) {
            return Err(PolicyRejected);
        }

        let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
        let call = OutboundCall {
            message_id: command.request_id.as_str().to_owned(),
            action: operation.action.clone(),
            payload: operation.payload.clone(),
            correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                CorrelationId::new(command.request_id.as_str()).expect("request identity")
            }),
        };

        Ok(Prepared {
            query,
            call,
            // Bound monotonic arithmetic even if an external command uses an extreme UTC expiry.
            deadline: Instant::now() + remaining.min(std::time::Duration::from_hours(24)),
        })
    }

    /// Item counts and encoded size against what this station reported it accepts. A limit the
    /// station never reported keeps the request to a single item (N04.FR.09, N06.FR.04,
    /// N02.FR.07); nothing is probed, split or retried.
    fn within_limits(
        &self,
        command: &Command<Value>,
        query: &MonitoringQuery201,
        operation: &uob_contracts::PrivilegedOcppOperation<Value>,
    ) -> bool {
        let Some(bytes) = json_size(
            &(
                2,
                command.request_id.as_str(),
                operation.action.as_str(),
                &operation.payload,
            ),
            FRAME_BYTES,
        ) else {
            return false;
        };
        let limits = self
            .configuration_limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match query {
            MonitoringQuery201::SetVariableMonitoring { monitors } => {
                limits.allows_monitoring(3, monitors.len(), bytes)
            }
            MonitoringQuery201::ClearVariableMonitoring { ids } => {
                limits.allows_monitoring(4, ids.len(), bytes)
            }
            MonitoringQuery201::GetMonitoringReport {
                component_variables,
                ..
            } => limits.allows_report_selectors(component_variables.len(), bytes),
            MonitoringQuery201::SetMonitoringBase { .. }
            | MonitoringQuery201::SetMonitoringLevel { .. } => bytes <= FRAME_BYTES,
        }
    }
}

async fn receive(query: &MonitoringQuery201, pending: PendingCall) -> CommandDispatchOutcome {
    match pending.receive().await {
        SessionCallOutcome::Result { payload, .. } => match reply::response(query, &payload) {
            Some(result) => CommandDispatchOutcome::MonitoringResponse201(Box::new(result)),
            None => mapping::uncertain(),
        },
        SessionCallOutcome::Error { .. } => mapping::rejected_response(),
        SessionCallOutcome::NotTransmitted { reason, .. } => {
            mapping::not_sent(if reason == "command expired before socket send" {
                CommandErrorCode::Expired
            } else {
                CommandErrorCode::PolicyRejected
            })
        }
        SessionCallOutcome::TimedOut { .. } | SessionCallOutcome::TransmissionUncertain { .. } => {
            mapping::uncertain()
        }
    }
}
