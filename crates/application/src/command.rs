mod admission;
mod charging_profile201;
mod configuration;
pub mod device_model201;
mod errors;
mod finalization;
mod local_authorization16;
mod local_authorization201;
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
    /// Immutable full native OCPP 1.6 profile request and valid acknowledgement.
    ChargingProfileResponse16(uob_contracts::ChargingProfileResult16),
    /// Immutable full native OCPP 2.0.1 profile request and valid acknowledgement.
    ChargingProfileResponse201(uob_contracts::ChargingProfileResult201),
    /// Value-free native configuration write acknowledgement.
    ConfigurationResponse201(uob_contracts::ConfigurationResult201),
    /// Exact native station list/cache response without private authorization material.
    LocalAuthorizationResponse16(uob_contracts::LocalAuthorizationResult16),
    LocalAuthorizationResponse201(uob_contracts::LocalAuthorizationResult201),
    ReservationResponse16(uob_contracts::ReservationResult16),
    ReservationResponse201(uob_contracts::ReservationResult201),
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

    /// Capture a validated native201 or canonical201 profile mutation before durable admission.
    /// Non-profile commands return None; profile producers must supply ownership metadata.
    ///
    /// # Errors
    /// Rejects invalid or unavailable resource/generation context and unsupported profile producers.
    fn charging_profile_expectation(
        &self,
        command: &Command<P>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<crate::ProfileReservation201>, CommandErrorCode>;
    /// Captures protected native16 mutations; unrelated operations return None.
    ///
    /// # Errors
    /// Rejects invalid, unprivileged or unavailable reservation context before admission.
    fn reservation_expectation_16(
        &self,
        _command: &Command<P>,
        _generation: Option<u64>,
        _now: UtcTimestamp,
    ) -> Result<Option<crate::ReservationMutation16>, CommandErrorCode> {
        Ok(None)
    }
    /// Captures protected native201 mutations; unrelated operations return None.
    ///
    /// # Errors
    /// Rejects invalid, unprivileged or unavailable reservation context before admission.
    fn reservation_expectation_201(
        &self,
        _command: &Command<P>,
        _generation: Option<u64>,
        _now: UtcTimestamp,
    ) -> Result<Option<crate::ReservationMutation201>, CommandErrorCode> {
        Ok(None)
    }
    /// Dispatches a durably reserved profile against the captured generation.
    /// Ports that do not support native201 fail closed rather than dropping the reservation.
    fn dispatch_reserved_profile(
        &self,
        _command: Command<P>,
        _generation: Option<u64>,
        _reservation: crate::ProfileReservation201,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async {
            Err(StationCommandError::new(
                "native201 profile dispatch unsupported",
            ))
        })
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
