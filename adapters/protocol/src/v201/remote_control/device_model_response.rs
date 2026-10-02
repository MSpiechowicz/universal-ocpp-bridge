use super::{
    device_model::learn_durable,
    device_model_collection::json_size,
    device_model_values::{self as values, LearnedLimits},
    mapping,
};
use crate::command_registry::device_model201::valid_schema;
use crate::{PendingCall, SessionCallOutcome, TransmissionUncertainReason};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use uob_application::{
    CommandClock, CommandDispatchOutcome, DeviceModelStore201, RuntimeResourceBudget, WorkClass,
};
use uob_contracts::{
    CommandErrorCode, CommandLifecycle, DEVICE_MODEL_OUTPUT_LIMIT_201, DeviceModelQuery201,
    DeviceModelResult201, DeviceReportAck201, DeviceReportFailure201, DeviceReportState201,
    RequestId, ResourceRef,
};

pub(super) struct ReplyWriter201 {
    pub store: Arc<dyn DeviceModelStore201>,
    pub clock: Arc<dyn CommandClock>,
    pub limits: Arc<Mutex<LearnedLimits>>,
    pub budget: RuntimeResourceBudget,
    pub request: RequestId,
    pub resource: ResourceRef,
    pub evidence: DeviceModelResult201,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
    pub _metadata: Arc<uob_application::RuntimeReservation>,
    pub handle: crate::CallSessionHandle,
    pub phase: Arc<Mutex<super::phase_capability::PhaseCapabilities>>,
    pub phase_sequence: u64,
}
impl ReplyWriter201 {
    pub async fn receive(mut self, pending: PendingCall) -> CommandDispatchOutcome {
        let (reply, _incoming) = pending.receive_guarded().await;
        let (full_output, mut failure) = self.parse_ack(&reply);
        let metadata_bytes = json_size(&self.evidence, DEVICE_MODEL_OUTPUT_LIMIT_201)
            .unwrap_or(DEVICE_MODEL_OUTPUT_LIMIT_201);
        let bytes = if full_output {
            DEVICE_MODEL_OUTPUT_LIMIT_201 * 3
        } else {
            metadata_bytes * 3 + 4096
        };
        let retained = self.budget.try_reserve(WorkClass::DatabaseWork, bytes).ok();
        self.apply_reply(reply, retained.is_some(), &mut failure);
        let retained = retained.or_else(|| {
            let bytes = json_size(&self.evidence, DEVICE_MODEL_OUTPUT_LIMIT_201)? * 3 + 4096;
            self.budget.try_reserve(WorkClass::DatabaseWork, bytes).ok()
        });
        let mut lifecycle = response_lifecycle(&self.evidence, failure.as_ref());
        let fallback = storage_fallback(&self.evidence);
        let mut persisted = self
            .store
            .finish_device_report(
                self.request.clone(),
                self.evidence,
                Some(lifecycle.clone()),
                self.clock.now(),
            )
            .await;
        // Only this generation's correlated, validated and durably committed reply grants proof.
        let variables = match &persisted {
            Ok(Some(result)) if failure.is_none() => result
                .device_model_201
                .as_ref()
                .map(|evidence| evidence.variables.as_slice()),
            _ => None,
        };
        self.phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(self.phase_sequence, &fallback.query, variables);
        if persisted.is_err() || matches!(persisted, Ok(None)) {
            self.handle.report_store_failed(fallback.connection.clone());
            if fallback.query.request_id().is_none() {
                failure = Some(mapping::uncertain());
                lifecycle = CommandLifecycle::TransmissionUncertain {
                    detail: "native device-model evidence could not be durably stored".to_owned(),
                };
            }
            persisted = self
                .store
                .finish_device_report(self.request, fallback, Some(lifecycle), self.clock.now())
                .await;
        }
        let continue_collection = matches!(&persisted, Ok(Some(result))
            if result.device_model_201.as_ref().is_some_and(|evidence|
                evidence.native_ack == Some(DeviceReportAck201::Accepted) && evidence.report.pending()));
        // Persist the precise native failure before cancellation contributes its accepted counts.
        if !continue_collection && let Some(stop) = self.stop {
            let _ = stop.send(());
        }
        let outcome = match persisted {
            Ok(Some(mut result)) => {
                learn_durable(&self.limits, &result);
                if let Some(failure) = failure {
                    failure
                } else {
                    let Some(mut evidence) = result.device_model_201.take() else {
                        return mapping::uncertain();
                    };
                    // This outcome carries native ACK evidence. The durable collector owns inventory.
                    if matches!(evidence.report, DeviceReportState201::Complete { .. }) {
                        evidence.report = DeviceReportState201::Pending;
                    }
                    CommandDispatchOutcome::DeviceModelResponse201(evidence)
                }
            }
            _ => mapping::uncertain(),
        };
        drop(retained);
        outcome
    }

    fn parse_ack(&mut self, reply: &SessionCallOutcome) -> (bool, Option<CommandDispatchOutcome>) {
        let mut failure = None;
        let mut full_output = false;
        // Native report ACK parsing copies no vendor fields. Inventory promotion is reserved below.
        if let SessionCallOutcome::Result { payload, .. } = reply {
            match &self.evidence.query {
                DeviceModelQuery201::GetVariables { .. } => full_output = true,
                query => {
                    let index = if matches!(query, DeviceModelQuery201::GetBaseReport { .. }) {
                        3
                    } else {
                        5
                    };
                    if valid_schema(index, payload) {
                        self.evidence.native_ack =
                            DeviceReportAck201::deserialize(&payload["status"]).ok();
                        full_output =
                            self.evidence.native_ack == Some(DeviceReportAck201::Accepted);
                    } else {
                        failure = Some(mapping::uncertain());
                    }
                }
            }
        }
        (full_output, failure)
    }

    fn apply_reply(
        &mut self,
        reply: SessionCallOutcome,
        retained: bool,
        failure: &mut Option<CommandDispatchOutcome>,
    ) {
        match reply {
            SessionCallOutcome::Result { payload, .. } => {
                if let DeviceModelQuery201::GetVariables { entries } = &self.evidence.query {
                    if retained
                        && let Some(results) = values::variables(entries, &payload, &self.resource)
                    {
                        self.evidence.variables = results;
                    } else {
                        *failure = Some(mapping::uncertain());
                    }
                } else if !retained
                    && failure.is_none()
                    && self.evidence.native_ack == Some(DeviceReportAck201::Accepted)
                {
                    self.evidence.report = DeviceReportState201::Incomplete {
                        reason: DeviceReportFailure201::Capacity,
                        progress: None,
                    };
                }
            }
            SessionCallOutcome::NotTransmitted { .. } => {
                *failure = Some(mapping::not_sent(CommandErrorCode::PolicyRejected));
            }
            SessionCallOutcome::Error { .. } => {
                *failure = Some(mapping::rejected_response());
            }
            SessionCallOutcome::TransmissionUncertain {
                reason:
                    TransmissionUncertainReason::Disconnected
                    | TransmissionUncertainReason::SessionStopped,
                ..
            } => {
                if self.evidence.query.request_id().is_some() {
                    self.evidence.report = DeviceReportState201::Incomplete {
                        reason: DeviceReportFailure201::Disconnected,
                        progress: None,
                    };
                }
                *failure = Some(mapping::uncertain());
            }
            _ => {
                *failure = Some(mapping::uncertain());
            }
        }
    }
}

fn response_lifecycle(
    evidence: &DeviceModelResult201,
    failure: Option<&CommandDispatchOutcome>,
) -> CommandLifecycle {
    match failure {
        None => CommandLifecycle::ProtocolResponse {
            accepted: evidence
                .native_ack
                .is_none_or(|ack| ack == DeviceReportAck201::Accepted),
            error: None,
        },
        Some(CommandDispatchOutcome::NotTransmitted { error }) => CommandLifecycle::Rejected {
            error: error.clone(),
        },
        Some(CommandDispatchOutcome::ProtocolResponse { accepted, error }) => {
            CommandLifecycle::ProtocolResponse {
                accepted: *accepted,
                error: error.clone(),
            }
        }
        _ => CommandLifecycle::TransmissionUncertain {
            detail: "native device-model response unavailable or malformed".to_owned(),
        },
    }
}

fn storage_fallback(evidence: &DeviceModelResult201) -> DeviceModelResult201 {
    DeviceModelResult201 {
        query: evidence.query.clone(),
        connection: evidence.connection.clone(),
        generation: evidence.generation,
        dispatch_recorded_at: evidence.dispatch_recorded_at,
        native_ack: evidence.native_ack,
        variables: Vec::new(),
        report: match evidence.native_ack {
            Some(DeviceReportAck201::EmptyResultSet) => DeviceReportState201::NotExpected,
            Some(DeviceReportAck201::Rejected | DeviceReportAck201::NotSupported) => {
                DeviceReportState201::Incomplete {
                    reason: DeviceReportFailure201::NativeRejected,
                    progress: None,
                }
            }
            _ if evidence.query.request_id().is_some() => match evidence.report {
                DeviceReportState201::Incomplete { reason, progress } => {
                    DeviceReportState201::Incomplete { reason, progress }
                }
                _ => DeviceReportState201::Incomplete {
                    reason: DeviceReportFailure201::StorageUnavailable,
                    progress: None,
                },
            },
            _ => DeviceReportState201::NotExpected,
        },
    }
}
