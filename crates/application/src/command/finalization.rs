use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandResult, ContractVersion,
    UtcTimestamp,
};

use super::{
    CommandCoordinator, CommandDispatchOutcome, TriggerExpectation, command_result,
    integrity_error, map_station_error, map_storage_error,
};
use crate::{CommandAdmissionError, FlowEvidence, FlowStage};

impl<P, E, D, R> CommandCoordinator<P, E, D, R>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    #[allow(clippy::too_many_lines)] // Preserve ordered response, persistence and reconciliation.
    pub(super) async fn finalize_dispatch(
        &self,
        command: Command<P>,
        generation: Option<u64>,
        trigger: Option<TriggerExpectation>,
        profile: Option<Box<crate::ProfileReservation201>>,
        dispatch_started_at: UtcTimestamp,
        trace: crate::FlowSpan,
    ) -> Result<CommandResult, CommandAdmissionError> {
        let mut config_response = None;
        let mut trigger_response = None;
        let mut trigger_response_201 = None;
        let mut composite_schedule = None;
        let mut device_model = None;
        let mut charging_profile = None;
        let mut charging_profile_201 = None;
        let mut configuration_201 = None;
        let mut local_authorization_16 = None;
        let mut local_authorization_201 = None;
        let outcome = if let Some(reservation) = profile {
            self.stations
                .dispatch_reserved_profile(command.clone(), generation, *reservation)
                .await
        } else {
            self.stations
                .dispatch_to_generation(command.clone(), generation)
                .await
        }
        .map_err(|error| map_station_error(&error))?;
        let lifecycle = match outcome {
            CommandDispatchOutcome::NotTransmitted { error } => {
                trace.emit_fields(
                    FlowStage::ProtocolResponse,
                    FlowEvidence::NotTransmitted,
                    vec![crate::SafeDiagnosticField::CommandReason(error.code)],
                );
                CommandLifecycle::Rejected { error }
            }
            CommandDispatchOutcome::ProtocolResponse { accepted, error } => {
                trace.emit_fields(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                    error
                        .as_ref()
                        .map(|error| crate::SafeDiagnosticField::CommandReason(error.code))
                        .into_iter()
                        .collect(),
                );
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::ConfigurationResponse {
                accepted,
                error,
                configuration,
            } => {
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                config_response = Some(configuration);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::TriggerResponse(response) => {
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if response == uob_contracts::TriggerNativeResponse::Accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                trigger_response = Some(response);
                CommandLifecycle::ProtocolResponse {
                    accepted: response == uob_contracts::TriggerNativeResponse::Accepted,
                    error: None,
                }
            }
            CommandDispatchOutcome::TriggerResponse201(response) => {
                let accepted = response.status == uob_contracts::TriggerNativeStatus201::Accepted;
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                trigger_response_201 = Some(response);
                CommandLifecycle::ProtocolResponse {
                    accepted,
                    error: None,
                }
            }
            CommandDispatchOutcome::CompositeScheduleResponse16(response) => {
                let accepted =
                    response.status == uob_contracts::CompositeScheduleStatus16::Accepted;
                let error = (!accepted).then(|| CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: Some("Rejected".to_owned()),
                });
                trace.emit_fields(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                    error
                        .as_ref()
                        .map(|error| crate::SafeDiagnosticField::CommandReason(error.code))
                        .into_iter()
                        .collect(),
                );
                composite_schedule = Some(response);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::DeviceModelResponse201(evidence) => {
                // Mixed explicit variable statuses are a valid native response.
                let accepted = evidence
                    .native_ack
                    .is_none_or(|ack| ack == uob_contracts::DeviceReportAck201::Accepted);
                device_model = Some(evidence);
                CommandLifecycle::ProtocolResponse {
                    accepted,
                    error: None,
                }
            }
            CommandDispatchOutcome::ChargingProfileResponse16(response) => {
                let accepted = response.accepted();
                let error = (!accepted).then_some(CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: None,
                });
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                charging_profile = Some(response);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::ChargingProfileResponse201(response) => {
                let accepted = response.accepted();
                let error = (!accepted).then_some(CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: None,
                });
                charging_profile_201 = Some(response);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::ConfigurationResponse201(response) => {
                let accepted = response.accepted();
                let error = (!accepted).then_some(CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: None,
                });
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                configuration_201 = Some(response);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::LocalAuthorizationResponse16(response) => {
                if !super::local_authorization16::valid_evidence(&command, &response) {
                    return Err(integrity_error(
                        "local authorization response has no matching request",
                    ));
                }
                let accepted = response.accepted();
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                let error = (!accepted).then_some(CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: None,
                });
                local_authorization_16 = Some(response);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::LocalAuthorizationResponse201(response) => {
                if !super::local_authorization201::valid_evidence(&command, &response) {
                    return Err(integrity_error(
                        "local authorization response has no matching request",
                    ));
                }
                let accepted = response.accepted();
                let error = (!accepted).then_some(CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: None,
                });
                local_authorization_201 = Some(response);
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::TransmissionUncertain { detail } => {
                trace.emit(FlowStage::ProtocolResponse, FlowEvidence::Uncertain);
                CommandLifecycle::TransmissionUncertain { detail }
            }
        };
        if (trigger_response.is_some() && !matches!(trigger, Some(TriggerExpectation::Ocpp16(_))))
            || (trigger_response_201.is_some()
                && !matches!(trigger, Some(TriggerExpectation::Ocpp201(_))))
        {
            return Err(integrity_error(
                "trigger response has no matching edition expectation",
            ));
        }
        let mut result = command_result(&command, lifecycle, self.clock.now());
        if let Some(configuration) = config_response {
            result.schema_version = ContractVersion::V1_CONFIGURATION;
            result.configuration = Some(configuration);
        }
        if let Some(schedule) = composite_schedule {
            result.schema_version = ContractVersion::V1_COMPOSITE_SCHEDULE_16;
            result.composite_schedule_16 = Some(schedule);
        }
        if let Some(evidence) = device_model {
            result.schema_version = ContractVersion::V1_DEVICE_MODEL_201;
            result.device_model_201 = Some(evidence);
        }
        if let Some(evidence) = charging_profile {
            result.schema_version = ContractVersion::V1_CHARGING_PROFILE_16;
            result.charging_profile_16 = Some(evidence);
        }
        if let Some(evidence) = charging_profile_201 {
            result.schema_version = ContractVersion::V1_CHARGING_PROFILE_201;
            result.charging_profile_201 = Some(evidence);
        }
        if let Some(evidence) = configuration_201 {
            result.schema_version = ContractVersion::V1_CONFIGURATION_201;
            result.configuration_201 = Some(evidence);
        }
        if let Some(evidence) = local_authorization_16 {
            result.schema_version = ContractVersion::V1_LOCAL_AUTHORIZATION_16;
            result.local_authorization_16 = Some(evidence);
        }
        if let Some(evidence) = local_authorization_201 {
            result.schema_version = ContractVersion::V1_LOCAL_AUTHORIZATION_201;
            result.local_authorization_201 = Some(evidence);
        }
        if let Some(expectation) = trigger.as_ref()
            && !matches!(result.lifecycle, CommandLifecycle::Rejected { .. })
        {
            expectation.finish(
                &mut result,
                dispatch_started_at,
                self.clock.now(),
                trigger_response,
                trigger_response_201,
            )?;
        }
        self.persist_result(result).await?;
        if trigger.is_some() {
            return self
                .store
                .reconcile_trigger_observation(command.request_id.clone(), self.clock.now())
                .await
                .map_err(|error| map_storage_error(&error))?
                .ok_or_else(|| integrity_error("completed trigger has no durable result"));
        }
        self.store
            .command_result_by_request_id(command.request_id.clone())
            .await
            .map_err(|error| map_storage_error(&error))?
            .ok_or_else(|| integrity_error("completed command has no durable result"))
    }
}
