use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandResult,
    CommandValidationError, Connectivity, ContractVersion, ExternalCommand, ObservedCommandEffect,
    RequestId, ResourceCapabilities, ResourceRef, UtcTimestamp,
};

use crate::{
    AtomicStoreWrite, CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture,
    CommandAdmissionOutcome, CommandAdmissionPort, FlowDiagnostics, FlowEvidence, FlowStage,
    OperationalStore, PageLimit, RecoveryQuery, StorageError, StorageWritePurpose,
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
    /// Restores unresolved command state without automatically dispatching recovered work.
    ///
    /// A persisted `Dispatched` state is conservatively converted to `TransmissionUncertain`
    /// because the process cannot prove whether the station acted before restart.
    #[must_use]
    pub fn recover_unresolved(
        &self,
        limit: PageLimit,
    ) -> CommandAdmissionFuture<'_, CommandRecoveryBatch<P>> {
        Box::pin(async move {
            let now = self.clock.now();
            let recovery = self
                .store
                .recover(RecoveryQuery { limit })
                .await
                .map_err(|error| map_storage_error(&error))?;
            let mut results = recovery
                .command_results
                .into_iter()
                .map(|result| (result.return_route.request_id.as_str().to_owned(), result))
                .collect::<BTreeMap<_, _>>();
            let mut commands = Vec::with_capacity(recovery.active_commands.len());
            for command in recovery.active_commands {
                let request_id = command.request_id.as_str();
                let mut result = results
                    .remove(request_id)
                    .unwrap_or_else(|| command_result(&command, CommandLifecycle::Admitted, now));
                if matches!(result.lifecycle, CommandLifecycle::Dispatched) {
                    result.lifecycle = CommandLifecycle::TransmissionUncertain {
                        detail: "service restarted before a charger response was recorded"
                            .to_owned(),
                    };
                    result.recorded_at = now;
                    self.persist_result(result.clone()).await?;
                }
                commands.push(RecoveredCommand { command, result });
            }
            Ok(CommandRecoveryBatch { commands })
        })
    }

    /// Links later observed state evidence without changing protocol acknowledgement state.
    #[must_use]
    pub fn reconcile_observed_effect(
        &self,
        request_id: RequestId,
        effect: ObservedCommandEffect,
    ) -> CommandAdmissionFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            let Some(command) = self
                .store
                .command_by_request_id(request_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?
            else {
                return Ok(None);
            };
            let mut result = self
                .store
                .command_result_by_request_id(request_id)
                .await
                .map_err(|error| map_storage_error(&error))?
                .unwrap_or_else(|| {
                    command_result(&command, CommandLifecycle::Admitted, self.clock.now())
                });
            if !result
                .observed_effects
                .iter()
                .any(|existing| existing.event_id == effect.event_id)
            {
                result.observed_effects.push(effect);
                self.persist_result(result.clone()).await?;
                self.diagnostics
                    .span(
                        result.correlation_id.clone(),
                        Some(result.resource.station_id.clone()),
                        None,
                    )
                    .emit(FlowStage::ObservedEffect, FlowEvidence::Observed);
            }
            Ok(Some(result))
        })
    }

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
        trace.emit(FlowStage::CommandIngress, FlowEvidence::Completed);
        let context = self
            .stations
            .context(external.request.resource.clone())
            .await
            .map_err(|error| map_station_error(&error))?;
        let connected = context
            .as_ref()
            .is_some_and(|value| matches!(value.connectivity, Connectivity::Connected { .. }));
        if !connected {
            trace.emit(FlowStage::Application, FlowEvidence::NotTransmitted);
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
        let lifecycle = match self
            .stations
            .dispatch(command.clone())
            .await
            .map_err(|error| map_station_error(&error))?
        {
            CommandDispatchOutcome::NotTransmitted { error } => {
                trace.emit(FlowStage::ProtocolResponse, FlowEvidence::NotTransmitted);
                CommandLifecycle::Rejected { error }
            }
            CommandDispatchOutcome::ProtocolResponse { accepted, error } => {
                trace.emit(
                    FlowStage::ProtocolResponse,
                    if accepted {
                        FlowEvidence::Accepted
                    } else {
                        FlowEvidence::Rejected
                    },
                );
                CommandLifecycle::ProtocolResponse { accepted, error }
            }
            CommandDispatchOutcome::TransmissionUncertain { detail } => {
                trace.emit(FlowStage::ProtocolResponse, FlowEvidence::Uncertain);
                CommandLifecycle::TransmissionUncertain { detail }
            }
        };
        let result = command_result(&command, lifecycle, self.clock.now());
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

fn map_storage_error(error: &StorageError) -> CommandAdmissionError {
    let code = match error.code() {
        crate::StorageErrorCode::Conflict | crate::StorageErrorCode::InvalidRequest => {
            CommandAdmissionErrorCode::InvalidRequest
        }
        crate::StorageErrorCode::Busy => CommandAdmissionErrorCode::Busy,
        crate::StorageErrorCode::CapacityExhausted => {
            CommandAdmissionErrorCode::StorageCapacityExhausted
        }
        _ => CommandAdmissionErrorCode::Unavailable,
    };
    CommandAdmissionError::new(code, error.detail())
}

fn map_station_error(error: &StationCommandError) -> CommandAdmissionError {
    CommandAdmissionError::new(CommandAdmissionErrorCode::Unavailable, error.context())
}

fn integrity_error(context: &str) -> CommandAdmissionError {
    CommandAdmissionError::new(CommandAdmissionErrorCode::Unavailable, context)
}
