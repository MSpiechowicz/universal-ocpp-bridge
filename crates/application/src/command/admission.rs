use super::{
    AtomicStoreWrite, CommandAdmissionError, CommandAdmissionOutcome, CommandCoordinator,
    CommandError, CommandErrorCode, CommandLifecycle, CommandResult, CommandValidationError,
    Connectivity, ExternalCommand, FlowEvidence, FlowStage, StorageWritePurpose, UtcTimestamp,
    charging_profile201, command_result, integrity_error, local_authorization16,
    local_authorization201, map_station_error, map_storage_error, rejected_external,
    valid_configuration_read, valid_protected_change, validation_rejection,
};

impl<P, E, D, R> CommandCoordinator<P, E, D, R>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    #[allow(clippy::too_many_lines)] // Keep the ordered admission/dispatch evidence beside each decision.
    pub(super) async fn submit_at(
        &self,
        external: ExternalCommand<P>,
        now: UtcTimestamp,
    ) -> Result<CommandResult, CommandAdmissionError> {
        let trace = self.diagnostics.span(
            external.request.correlation_id.clone(),
            Some(external.request.resource.station_id.clone()),
            None,
        );
        let trace = trace.with_request(external.request.request_id.clone());
        trace.emit_fields(
            FlowStage::CommandIngress,
            FlowEvidence::Completed,
            vec![crate::SafeDiagnosticField::CommandOrigin(
                external.origin.clone(),
            )],
        );
        // Idempotency is durable identity, not another admission attempt. Compare the complete
        // authenticated request before consulting the transient socket or its expiry deadline.
        if let Some(existing) = self
            .store
            .command_by_request_id(external.request.request_id.clone())
            .await
            .map_err(|error| map_storage_error(&error))?
        {
            let mut candidate = external.clone().admit(existing.admitted_at);
            candidate.schema_version = existing.schema_version;
            if candidate != existing {
                return Err(CommandAdmissionError::new(
                    crate::CommandAdmissionErrorCode::InvalidRequest,
                    "request ID is already associated with another command",
                ));
            }
            trace.emit(FlowStage::Deduplication, FlowEvidence::Duplicate);
            return self
                .store
                .command_result_by_request_id(existing.request_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?
                .ok_or_else(|| integrity_error("admitted command has no durable result"));
        }
        let invalid_local = match &external.request.operation {
            uob_contracts::CommandOperation::Ocpp(operation)
                if operation.protocol == uob_contracts::ProtocolEdition::Ocpp201 =>
            {
                local_authorization201::invalid(
                    &external.request.resource,
                    &external.request.operation,
                )
            }
            _ => local_authorization16::invalid(
                &external.request.resource,
                &external.request.operation,
            ),
        };
        if invalid_local {
            return Ok(rejected_external(
                &external,
                CommandErrorCode::InvalidParameters,
                "invalid protected local authorization request",
                now,
            ));
        }
        if let uob_contracts::CommandOperation::Ocpp(operation) = &external.request.operation {
            if operation.protocol == uob_contracts::ProtocolEdition::Ocpp16j
                && operation.action.as_str() == "ChangeConfiguration"
                && !valid_protected_change(operation)
            {
                return Ok(rejected_external(
                    &external,
                    CommandErrorCode::InvalidParameters,
                    "configuration value must use a protected reference",
                    now,
                ));
            }
            if operation.protocol == uob_contracts::ProtocolEdition::Ocpp16j
                && operation.action.as_str() == "GetConfiguration"
                && !valid_configuration_read(operation)
            {
                return Ok(rejected_external(
                    &external,
                    CommandErrorCode::InvalidParameters,
                    "invalid configuration read request",
                    now,
                ));
            }
        }
        if now >= external.request.expires_at {
            trace.emit(FlowStage::Validation, FlowEvidence::Rejected);
            return Ok(validation_rejection(
                &external.clone().admit(now),
                &CommandValidationError::Expired,
                now,
            ));
        }
        let generation = self.stations.session_generation(&external.request.resource);
        let context = self
            .stations
            .context(external.request.resource.clone())
            .await
            .map_err(|error| map_station_error(&error))?;
        let connected = context
            .as_ref()
            .is_some_and(|value| matches!(value.connectivity, Connectivity::Connected { .. }));
        if !connected {
            trace.emit_fields(
                FlowStage::Application,
                FlowEvidence::NotTransmitted,
                vec![crate::SafeDiagnosticField::CommandReason(
                    CommandErrorCode::StationDisconnected,
                )],
            );
            return Ok(rejected_external(
                &external,
                CommandErrorCode::StationDisconnected,
                "station is not connected",
                now,
            ));
        }
        let command = external.admit(now);
        if let Err(error) =
            command.validate_for_dispatch(&context.expect("connected context").capabilities, now)
        {
            trace.emit(FlowStage::Validation, FlowEvidence::Rejected);
            return Ok(validation_rejection(&command, &error, now));
        }
        let mut device_model = match self
            .stations
            .device_model_expectation(&command, generation, now)
        {
            Ok(evidence) => evidence,
            Err(code) => {
                return Ok(command_result(
                    &command,
                    CommandLifecycle::Rejected {
                        error: CommandError {
                            code,
                            detail: Some("invalid native device-model query".to_owned()),
                        },
                    },
                    now,
                ));
            }
        };
        trace.emit(FlowStage::Validation, FlowEvidence::Completed);
        let admitted = command_result(&command, CommandLifecycle::Admitted, now);
        let mut write = AtomicStoreWrite::empty();
        write.purpose = match command.operation {
            uob_contracts::CommandOperation::Start { .. } => StorageWritePurpose::NewSessionStart,
            uob_contracts::CommandOperation::Stop { .. } => {
                StorageWritePurpose::ActiveSessionCompletion
            }
            _ => StorageWritePurpose::Routine,
        };
        write.command = Some(command.clone());
        write.command_result = Some(admitted.clone());
        write.charging_profile_201 = match self
            .stations
            .charging_profile_expectation(&command, generation, now)
        {
            Ok(reservation) => reservation.map(Box::new),
            Err(code) => return Ok(charging_profile201::rejected(&command, code, now)),
        };
        write.reservation_16 = match self
            .stations
            .reservation_expectation_16(&command, generation, now)
        {
            Ok(mutation) => mutation.map(Box::new),
            Err(code) => return Ok(charging_profile201::rejected(&command, code, now)),
        };
        write.reservation_201 = match self
            .stations
            .reservation_expectation_201(&command, generation, now)
        {
            Ok(mutation) => mutation.map(Box::new),
            Err(code) => return Ok(charging_profile201::rejected(&command, code, now)),
        };
        write.firmware_16 = match self
            .stations
            .firmware_expectation_16(&command, generation, now)
        {
            Ok(mutation) => mutation.map(Box::new),
            Err(code) => return Ok(charging_profile201::rejected(&command, code, now)),
        };
        // A firmware job must be registered durably before any byte can reach the station.
        if write.firmware_16.is_none() && firmware_action(&command.operation) {
            return Ok(charging_profile201::rejected(
                &command,
                CommandErrorCode::UnsupportedOperation,
                now,
            ));
        }
        let reservation_mutation = write.reservation_16.is_some()
            || write.reservation_201.is_some()
            || write.firmware_16.is_some();
        let profile = write.charging_profile_201.clone();
        let outcome = match self.store.write_atomic(write).await {
            Ok(outcome) => outcome,
            Err(error)
                if (profile.is_some() || reservation_mutation)
                    && error.code() == crate::StorageErrorCode::Conflict =>
            {
                return Ok(charging_profile201::rejected_detail(
                    &command,
                    CommandErrorCode::PolicyRejected,
                    now,
                    error.detail(),
                ));
            }
            Err(error) => return Err(map_storage_error(&error)),
        };
        match outcome.command {
            Some(CommandAdmissionOutcome::Duplicate {
                result: Some(result),
            }) => {
                trace.emit(FlowStage::Deduplication, FlowEvidence::Duplicate);
                return Ok(*result);
            }
            Some(CommandAdmissionOutcome::Duplicate { result: None }) => {
                trace.emit(FlowStage::Deduplication, FlowEvidence::Duplicate);
                return Ok(self
                    .store
                    .command_result_by_request_id(command.request_id.clone())
                    .await
                    .map_err(|error| map_storage_error(&error))?
                    .unwrap_or(admitted));
            }
            Some(CommandAdmissionOutcome::Admitted) => {}
            None => return Err(integrity_error("storage omitted command admission outcome")),
        }

        trace.emit(FlowStage::DurableCommit, FlowEvidence::Completed);
        let trigger = self.stations.trigger_expectation(&command);
        let dispatch_started_at = self.clock.now();
        let mut dispatched =
            command_result(&command, CommandLifecycle::Dispatched, dispatch_started_at);
        if let Some(mut evidence) = device_model.take() {
            evidence.dispatch_recorded_at = dispatch_started_at;
            dispatched.schema_version = uob_contracts::ContractVersion::V1_DEVICE_MODEL_201;
            dispatched.device_model_201 = Some(evidence);
        }
        if let Some(expectation) = trigger.as_ref() {
            expectation.start(&mut dispatched, dispatch_started_at)?;
        }
        self.persist_result(dispatched).await?;
        trace.emit(FlowStage::CommandDispatch, FlowEvidence::Completed);
        self.finalize_dispatch(
            command,
            generation,
            trigger,
            profile,
            dispatch_started_at,
            trace,
        )
        .await
    }
}

fn firmware_action<P>(operation: &uob_contracts::CommandOperation<P>) -> bool {
    matches!(operation, uob_contracts::CommandOperation::Ocpp(operation)
        if operation.protocol == uob_contracts::ProtocolEdition::Ocpp16j
            && matches!(operation.action.as_str(), "UpdateFirmware" | "SignedUpdateFirmware"))
}
