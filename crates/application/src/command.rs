mod configuration;
pub mod device_model201;
mod errors;
mod finalization;
mod recovery;
mod results;
use results::{command_result, rejected_external, validation_rejection};
mod trigger;
mod trigger201;
use configuration::{valid_configuration_read, valid_protected_change};
use errors::{integrity_error, map_station_error, map_storage_error};
use std::{future::Future, pin::Pin, sync::Arc};
pub use trigger::{TriggerExpectation, TriggerExpectation16};
pub use trigger201::TriggerExpectation201;

use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandResult,
    CommandValidationError, Connectivity, ExternalCommand, ResourceCapabilities, ResourceRef,
    UtcTimestamp,
};

use crate::{
    AtomicStoreWrite, CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture,
    CommandAdmissionOutcome, CommandAdmissionPort, FlowDiagnostics, FlowEvidence, FlowStage,
    OperationalStore, StorageError, StorageWritePurpose,
};

/// Future returned by the station command boundary.
pub type StationCommandFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, StationCommandError>> + Send + 'a>>;

/// Connection and capability facts read from the currently admitted station session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationCommandContext {
    /// Current observed connectivity.
    pub connectivity: Connectivity,
    /// Operations explicitly advertised for the addressed resource.
    pub capabilities: ResourceCapabilities,
}

/// Exact result of one dispatch attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandDispatchOutcome {
    /// The adapter proved that no command bytes were transmitted.
    NotTransmitted {
        /// Stable reason the command could not be sent.
        error: CommandError,
    },
    /// A correlated charger response was received.
    ProtocolResponse {
        /// Whether the charger accepted the protocol operation.
        accepted: bool,
        /// Stable rejection detail, when the charger rejected it.
        error: Option<CommandError>,
    },
    /// Validated, sanitized OCPP configuration response, separate from subsequent observations.
    ConfigurationResponse {
        accepted: bool,
        error: Option<CommandError>,
        configuration: uob_contracts::ConfigurationResult,
    },
    /// Transmission may have occurred but no correlated response was recorded.
    TransmissionUncertain {
        /// Sanitized reason that observation is required before further action.
        detail: String,
    },
    /// Exact OCPP 1.6 `TriggerMessage` native reply, never subsequent-message proof.
    TriggerResponse(uob_contracts::TriggerNativeResponse),
    /// Exact OCPP 2.0.1 reply including optional native statusInfo.
    TriggerResponse201(uob_contracts::TriggerNativeResponse201),
    /// Validated native OCPP 1.6 composite schedule and immutable query context.
    CompositeScheduleResponse16(uob_contracts::CompositeScheduleResult16),
    /// Validated native read evidence; collection has its own durable lifecycle.
    DeviceModelResponse201(uob_contracts::DeviceModelResult201),
}

/// Sanitized failure to inspect or use the current station session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationCommandError {
    context: String,
}

impl StationCommandError {
    /// Creates a failure from pre-sanitized context.
    #[must_use]
    pub fn new(context: impl Into<String>) -> Self {
        Self {
            context: context.into(),
        }
    }

    /// Returns sanitized context.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

/// Live station boundary used by the application-owned command coordinator.
pub trait StationCommandPort<P>: Send + Sync {
    /// Reads facts only from the currently admitted station session.
    fn context(
        &self,
        resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>>;

    /// Opaque socket generation captured before admission; adapters without socket turnover
    /// may leave this absent.
    fn session_generation(&self, _resource: &ResourceRef) -> Option<u64> {
        None
    }
    /// Explicit opt-in expectation captured before sending a `TriggerMessage`.
    fn trigger_expectation(&self, _command: &Command<P>) -> Option<TriggerExpectation> {
        None
    }
    /// Validate and capture native read context before durable admission.
    ///
    /// # Errors
    /// Returns `InvalidParameters` for an invalid or out-of-scope native query,
    /// `UnsupportedOperation` when native reads are not enabled, or
    /// `StationDisconnected` when the captured session is absent or no longer active.
    fn device_model_expectation(
        &self,
        _command: &Command<P>,
        _generation: Option<u64>,
        _now: UtcTimestamp,
    ) -> Result<Option<uob_contracts::DeviceModelResult201>, CommandErrorCode> {
        Ok(None)
    }

    /// Dispatches only to the socket observed before durable admission.
    fn dispatch_to_generation(
        &self,
        command: Command<P>,
        _generation: Option<u64>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        self.dispatch(command)
    }

    /// Attempts one dispatch against the same live-session registry.
    ///
    /// Implementations must classify whether bytes were definitely not sent or transmission is
    /// uncertain. They must never retain a command for dispatch on a later connection.
    fn dispatch(&self, command: Command<P>) -> StationCommandFuture<'_, CommandDispatchOutcome>;
}

/// Trusted UTC source injected by the composition root.
pub trait CommandClock: Send + Sync {
    /// Returns current UTC time for admission and lifecycle records.
    fn now(&self) -> UtcTimestamp;
}

/// One unresolved command restored without scheduling an automatic replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredCommand<P> {
    /// Original durable command.
    pub command: Command<P>,
    /// Latest durable lifecycle and independently observed effects.
    pub result: CommandResult,
}

/// Bounded command-only recovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRecoveryBatch<P> {
    /// Unresolved commands that require explicit lifecycle handling or observation.
    pub commands: Vec<RecoveredCommand<P>>,
}

/// Coordinates durable admission, one live dispatch attempt, and conservative recovery.
pub struct CommandCoordinator<P, E, D, R> {
    store: Arc<dyn OperationalStore<P, E, D, R>>,
    stations: Arc<dyn StationCommandPort<P>>,
    clock: Arc<dyn CommandClock>,
    diagnostics: FlowDiagnostics,
}

impl<P, E, D, R> CommandCoordinator<P, E, D, R> {
    /// Attaches the same process emitter used by protocol and target hosts.
    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: FlowDiagnostics) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Creates a coordinator from application-owned ports.
    #[must_use]
    pub fn new(
        store: Arc<dyn OperationalStore<P, E, D, R>>,
        stations: Arc<dyn StationCommandPort<P>>,
        clock: Arc<dyn CommandClock>,
    ) -> Self {
        Self {
            store,
            stations,
            clock,
            diagnostics: FlowDiagnostics::default(),
        }
    }
}

impl<P, E, D, R> CommandCoordinator<P, E, D, R>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    #[allow(clippy::too_many_lines)] // Keep the ordered admission/dispatch evidence beside each decision.
    async fn submit_at(
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
        let outcome = self
            .store
            .write_atomic(write)
            .await
            .map_err(|error| map_storage_error(&error))?;
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
        self.finalize_dispatch(command, generation, trigger, dispatch_started_at, trace)
            .await
    }

    async fn persist_result(&self, result: CommandResult) -> Result<(), CommandAdmissionError> {
        let mut write: AtomicStoreWrite<P, E, D, R> = AtomicStoreWrite::empty();
        write.purpose = StorageWritePurpose::ActiveSessionCompletion;
        write.command_result = Some(result);
        self.store
            .write_atomic(write)
            .await
            .map(|_| ())
            .map_err(|error| map_storage_error(&error))
    }
}

impl<P, E, D, R> CommandAdmissionPort<P> for CommandCoordinator<P, E, D, R>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    fn submit(&self, command: ExternalCommand<P>) -> CommandAdmissionFuture<'_, CommandResult> {
        let now = self.clock.now();
        Box::pin(self.submit_at(command, now))
    }
}
