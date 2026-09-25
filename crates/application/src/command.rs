mod configuration;
mod errors;
mod recovery;
use configuration::{valid_configuration_read, valid_protected_change};
use errors::{integrity_error, map_station_error, map_storage_error};
use std::{future::Future, pin::Pin, sync::Arc};

use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandResult,
    CommandValidationError, Connectivity, ContractVersion, ExternalCommand, ResourceCapabilities,
    ResourceRef, UtcTimestamp,
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
    P: Clone + Send + Sync + 'static,
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
        let dispatched = command_result(&command, CommandLifecycle::Dispatched, now);
        self.persist_result(dispatched).await?;
        trace.emit(FlowStage::CommandDispatch, FlowEvidence::Completed);
        let mut config_response = None;
        let lifecycle = match self
            .stations
            .dispatch(command.clone())
            .await
            .map_err(|error| map_station_error(&error))?
        {
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
            CommandDispatchOutcome::TransmissionUncertain { detail } => {
                trace.emit(FlowStage::ProtocolResponse, FlowEvidence::Uncertain);
                CommandLifecycle::TransmissionUncertain { detail }
            }
        };
        let mut result = command_result(&command, lifecycle, self.clock.now());
        if let Some(configuration) = config_response {
            result.schema_version = ContractVersion::V1_CONFIGURATION;
            result.configuration = Some(configuration);
        }
        self.persist_result(result.clone()).await?;
        Ok(result)
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
    P: Clone + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    fn submit(&self, command: ExternalCommand<P>) -> CommandAdmissionFuture<'_, CommandResult> {
        let now = self.clock.now();
        Box::pin(self.submit_at(command, now))
    }
}

fn command_result<P>(
    command: &Command<P>,
    lifecycle: CommandLifecycle,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at,
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
    }
}

fn rejected_external<P>(
    command: &ExternalCommand<P>,
    code: CommandErrorCode,
    detail: &str,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: uob_contracts::CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Rejected {
            error: CommandError {
                code,
                detail: Some(detail.to_owned()),
            },
        },
        recorded_at,
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
    }
}

fn validation_rejection<P>(
    command: &Command<P>,
    error: &CommandValidationError,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    let code = match error {
        CommandValidationError::Expired => CommandErrorCode::Expired,
        CommandValidationError::UnsupportedOperation(_) => CommandErrorCode::UnsupportedOperation,
    };
    command_result(
        command,
        CommandLifecycle::Rejected {
            error: CommandError {
                code,
                detail: Some(error.to_string()),
            },
        },
        recorded_at,
    )
}
