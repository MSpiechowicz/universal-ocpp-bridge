//! OCPP 2.0.1 remote operations behind the ordinary durable application command path.
use crate::remote_constraints as constraints;
mod charging_limit;
mod charging_profile201;
mod charging_profiles;
pub(crate) mod charging_profiles_report;
mod composite_schedule;
mod configuration201;
pub(crate) mod configuration201_profile_parse;
mod configuration201_response;
pub mod configuration201_values;
pub(crate) mod configuration201_wire;
pub mod device_model;
mod device_model_collection;
mod device_model_response;
pub mod device_model_values;
mod diagnostics;
mod firmware;
mod identity;
mod local_authorization;
mod local_authorization_language;
mod local_authorization_limits;
mod local_authorization_schema;
pub mod local_authorization_values;
pub(crate) mod local_authorization_wire;
mod mapping;
mod phase_capability;
mod reservation;
mod reservation_values;
pub(crate) mod schedule_values;
mod snapshot_guard;
mod trigger;
pub use identity::LocalRemoteStartIdentity;
pub use local_authorization_values::{
    LocalAuthorizationUpdates201, ProtectedLocalListUpdate201, ProtectedLocalListValue201,
};
pub(crate) use reservation::DeferredReservationCall201;
pub use reservation::ReservationGrant201;
pub(crate) use reservation::native_status as reservation_native_status_201;
pub use reservation::response as reservation_response_201;
pub use reservation_values::{ReservationValues201, reservation_key_201};
pub mod observation;

use crate::{CallSessionHandle, OutboundCall, PendingCall, SessionCallOutcome, SessionSubmitError};
pub use diagnostics::DiagnosticsSettings201;
pub use firmware::FirmwareSettings201;
use serde_json::Value;
use snapshot_guard::{connected_at, validate_snapshot};
use std::sync::{Arc, RwLock};
use tokio::time::Instant;
use uob_application::{
    CommandClock, CommandDispatchOutcome, StationCommandContext, StationCommandError,
    StationCommandFuture, StationCommandPort,
};
use uob_contracts::{
    Command, CommandErrorCode, Connectivity, ProtocolEdition, RequestId, ResourceRef,
    StationSnapshot, UtcTimestamp,
};

/// Narrow trusted provider for a remote start's opaque authorization reference.
/// Implementations must resolve only locally configured references, recheck current local policy
/// for the exact resource/time, and return no token on revocation, expiry or provider failure.
/// This synchronous port must be bounded and must not perform network or blocking I/O.
/// Typed idTokens are exposed only while constructing the socket payload, never persisted in commands.
pub trait RemoteStartIdentity: Send + Sync {
    fn authorized_token(
        &self,
        reference: &str,
        resource: &ResourceRef,
        now: UtcTimestamp,
    ) -> Option<uob_application::charging_identity::PresentedChargingIdentity>;
}

/// One admitted OCPP 2.0.1 socket and its latest committed snapshot. Construct a new port for a
/// reconnect; never replace its socket handle. The host's station registry routes exact resources
/// here and wraps its coordinator with scoped access and charging authorization guards.
pub struct RemoteControlSession {
    handle: CallSessionHandle,
    snapshot: Arc<RwLock<StationSnapshot>>,
    identity: Arc<dyn RemoteStartIdentity>,
    clock: Arc<dyn CommandClock>,
    evidence: Arc<dyn uob_application::remote_control::RemoteControlStore>,
    device_model: Option<device_model::DeviceRuntime>,
    phase: Arc<std::sync::Mutex<phase_capability::PhaseCapabilities>>,
    established_transactions: std::sync::Mutex<std::collections::BTreeSet<String>>,
    pending_snapshot_commit: std::sync::atomic::AtomicBool,
    configuration_201: Option<Arc<configuration201_values::LocalConfigurationValues201>>,
    configuration_limits: Arc<std::sync::Mutex<device_model_values::LearnedLimits>>,
    configuration_active: Arc<std::sync::Mutex<bool>>,
    local_authorization_updates: Option<Arc<LocalAuthorizationUpdates201>>,
    local_authorization_authority: [Arc<std::sync::Mutex<bool>>; 3],
    reservation_values: Option<Arc<reservation_values::ReservationValues201>>,
    reservation_grant: Option<Arc<ReservationGrant201>>,
    reserve_non_evse_specific: bool,
    profile_reports: Option<Arc<dyn uob_application::ChargingProfileReportStore201>>,
    firmware: Option<Arc<FirmwareSettings201>>,
    diagnostics: Option<Arc<DiagnosticsSettings201>>,
}

impl RemoteControlSession {
    /// Binds only a bounded snapshot matching the authenticated socket identity and edition.
    /// # Errors
    /// Rejects mismatched or oversized station state before accepting commands.
    pub fn new(
        handle: CallSessionHandle,
        snapshot: StationSnapshot,
        identity: Arc<dyn RemoteStartIdentity>,
        clock: Arc<dyn CommandClock>,
        evidence: Arc<dyn uob_application::remote_control::RemoteControlStore>,
    ) -> Result<Self, StationCommandError> {
        validate_snapshot(&handle, &snapshot)?;
        let phase = phase_capability::PhaseCapabilities::connected(&snapshot);
        let registered = uob_application::registration::v201::accepted(&snapshot).is_ok();
        let local_authorization_authority = std::array::from_fn(|index| {
            let enabled = registered
                && snapshot
                    .capabilities
                    .supports(&uob_contracts::Operation::ProtocolAction {
                        protocol: ProtocolEdition::Ocpp201,
                        action: crate::command_registry::local_authorization201::ACTIONS[index]
                            .to_owned(),
                    });
            Arc::new(std::sync::Mutex::new(enabled))
        });
        Ok(Self {
            handle,
            snapshot: Arc::new(RwLock::new(snapshot)),
            identity,
            clock,
            evidence,
            device_model: None,
            phase: Arc::new(std::sync::Mutex::new(phase)),
            established_transactions: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            pending_snapshot_commit: std::sync::atomic::AtomicBool::new(false),
            configuration_201: None,
            configuration_limits: Arc::new(std::sync::Mutex::new(
                device_model_values::LearnedLimits::default(),
            )),
            configuration_active: Arc::new(std::sync::Mutex::new(true)),
            local_authorization_updates: None,
            local_authorization_authority,
            reservation_values: None,
            reservation_grant: None,
            reserve_non_evse_specific: false,
            profile_reports: None,
            firmware: None,
            diagnostics: None,
        })
    }

    /// Fences profile enqueue before the host begins committing a transaction observation.
    /// `update_committed` releases the fence after publishing the committed snapshot.
    /// # Errors
    /// Poisoned snapshot state keeps dispatch failed closed.
    pub fn begin_snapshot_commit(&self) -> Result<(), StationCommandError> {
        let _snapshot = self.snapshot.write().map_err(|_| state_error())?;
        self.pending_snapshot_commit
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Releases an abandoned commit fence by invalidating this generation's profile authority.
    /// Invalid/duplicate observations instead publish unchanged committed state normally.
    /// A canceled or failed host must reconnect, never dispatch from possibly unpublished state.
    pub fn abort_snapshot_commit(&self) {
        if self
            .pending_snapshot_commit
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.detach_device_model();
        }
    }

    /// Publishes a snapshot only after the ordered station handler has committed it.
    /// # Errors
    /// Rejects relabeling, stale observations, reconnect epochs and poisoned state.
    pub fn update_committed(&self, snapshot: StationSnapshot) -> Result<(), StationCommandError> {
        validate_snapshot(&self.handle, &snapshot)?;
        let mut current = self.snapshot.write().map_err(|_| state_error())?;
        if snapshot.station != current.station
            || snapshot.observed_at < current.observed_at
            || connected_at(&snapshot) != connected_at(&current)
        {
            return Err(state_error());
        }
        self.learn_transactions(&current, &snapshot)?;
        let registered = uob_application::registration::v201::accepted(&snapshot).is_ok();
        for (index, authority) in self.local_authorization_authority.iter().enumerate() {
            *authority.lock().map_err(|_| state_error())? = registered
                && snapshot
                    .capabilities
                    .supports(&uob_contracts::Operation::ProtocolAction {
                        protocol: ProtocolEdition::Ocpp201,
                        action: crate::command_registry::local_authorization201::ACTIONS[index]
                            .to_owned(),
                    });
        }
        *current = snapshot;
        self.pending_snapshot_commit
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    async fn receive_remote_response(
        &self,
        request: &RequestId,
        action: &str,
        pending: PendingCall,
        profile: Option<crate::command_registry::charging_profile201::Request>,
        schedule: Option<uob_contracts::CompositeScheduleRequest201>,
    ) -> CommandDispatchOutcome {
        match pending.receive().await {
            SessionCallOutcome::Result { payload, .. } => {
                let outcome = if let Some(request) = schedule {
                    composite_schedule::response(request, &payload)
                } else if let Some(request) = profile {
                    charging_profile201::response(request, &payload)
                } else {
                    mapping::response(action, &payload)
                };
                if action != "SetChargingProfile"
                    && action != "TriggerMessage"
                    && matches!(outcome, CommandDispatchOutcome::ProtocolResponse { .. })
                {
                    let status = payload["status"]
                        .as_str()
                        .expect("validated status")
                        .to_owned();
                    let native = payload
                        .get("transactionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if self
                        .evidence
                        .record_remote_response(request.clone(), status, native)
                        .await
                        .is_err()
                    {
                        return mapping::uncertain();
                    }
                }
                outcome
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
    #[allow(clippy::too_many_lines)] // Owned families branch first; the shared path stays linear.
    pub(super) fn dispatch_profile(
        &self,
        command: Command<Value>,
        reservation: Option<uob_application::ProfileReservation201>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            if firmware::owns(&command) {
                return Ok(self.dispatch_firmware(&command).await);
            }
            if diagnostics::owns(&command) {
                return Ok(self.dispatch_diagnostics(&command).await);
            }
            if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
                if crate::command_registry::reservation201::ACTIONS.contains(&operation.action.as_str()))
            {
                return Ok(self.dispatch_reservation(command).await);
            }
            if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
                if crate::command_registry::local_authorization201::ACTIONS.contains(&operation.action.as_str()))
            {
                return Ok(self.dispatch_local_authorization(command).await);
            }
            if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
                if crate::command_registry::device_model201::ACTIONS.contains(&operation.action.as_str()))
            {
                return Ok(self.dispatch_device(command).await);
            }
            if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
                if crate::command_registry::configuration201::ACTIONS.contains(&operation.action.as_str()))
            {
                return Ok(self.dispatch_configuration_201(command).await);
            }
            if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
                if operation.action.as_str() == crate::command_registry::charging_profiles201::ACTION)
            {
                return Ok(self.dispatch_charging_profiles(command).await);
            }
            let schedule = match composite_schedule::request_context(&command) {
                Ok(schedule) => schedule,
                Err(code) => return Ok(mapping::not_sent(code)),
            };
            let remote_start_id = match self.reserve_start_id(&command).await {
                Ok(id) => id,
                Err(code) => return Ok(mapping::not_sent(code)),
            };
            // Snapshot/phase locks linearize commit and capability revocation against bounded enqueue.
            let (action, profile, pending) = {
                let snapshot = self.snapshot.read().map_err(|_| state_error())?;
                let phases = self.phase.lock().map_err(|_| state_error())?;
                if self.handle.is_closed() {
                    return Ok(mapping::not_sent(CommandErrorCode::StationDisconnected));
                }
                let context = match self.profile_context_with_phase(
                    &command,
                    &snapshot,
                    self.clock.now(),
                    &phases,
                ) {
                    Ok(context) => context,
                    Err(code) => return Ok(mapping::not_sent(code)),
                };
                if let Some(reserved) = &reservation
                    && (reserved.connection != self.handle.connection_id()
                        || reserved.request_id != command.request_id
                        || reserved.station != snapshot.station
                        || context
                            .as_ref()
                            .is_none_or(|(_, mutation)| mutation != &reserved.mutation))
                {
                    return Ok(mapping::not_sent(CommandErrorCode::PolicyRejected));
                }
                let prepared = mapping::prepare(
                    &command,
                    &snapshot,
                    self.identity.as_ref(),
                    self.clock.now(),
                    remote_start_id,
                )
                .map(|(action, payload)| {
                    (action, payload, context.and_then(|(request, _)| request))
                });
                let (action, payload, profile) = match prepared {
                    Ok(value) => value,
                    Err(code) => return Ok(mapping::not_sent(code)),
                };
                let now = self.clock.now();
                if now >= command.expires_at {
                    return Ok(mapping::not_sent(CommandErrorCode::Expired));
                }
                let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
                // Bound monotonic arithmetic even if an external command uses an extreme UTC expiry.
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
                let pending = match self.handle.try_call_before(call, deadline) {
                    Ok(pending) => pending,
                    Err(error) => {
                        return Ok(mapping::not_sent(match error {
                            SessionSubmitError::Closed => CommandErrorCode::StationDisconnected,
                            SessionSubmitError::InvalidRequest => {
                                CommandErrorCode::InvalidParameters
                            }
                            SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
                                CommandErrorCode::PolicyRejected
                            }
                        }));
                    }
                };
                (action, profile, pending)
            };
            Ok(self
                .receive_remote_response(&command.request_id, action, pending, profile, schedule)
                .await)
        })
    }

    async fn reserve_start_id(
        &self,
        command: &Command<Value>,
    ) -> Result<Option<i32>, CommandErrorCode> {
        if !matches!(
            command.operation,
            uob_contracts::CommandOperation::Start { .. }
        ) {
            return Ok(None);
        }
        self.evidence
            .reserve_remote_start(command.request_id.clone())
            .await
            .map(Some)
            .map_err(|_| CommandErrorCode::PolicyRejected)
    }
}

impl StationCommandPort<Value> for RemoteControlSession {
    fn reservation_expectation_201(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_application::ReservationMutation201>, CommandErrorCode> {
        self.reservation_context(command, generation, now)
    }
    fn firmware_expectation_201(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_application::FirmwareJobMutation201>, CommandErrorCode> {
        self.firmware_context(command, generation, now)
    }
    fn diagnostics_expectation_201(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_application::DiagnosticsJobMutation201>, CommandErrorCode> {
        self.diagnostics_context(command, generation, now)
    }
    fn charging_profile_expectation(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_application::ProfileReservation201>, CommandErrorCode> {
        self.profile_expectation(command, generation, now)
    }
    fn dispatch_reserved_profile(
        &self,
        command: Command<Value>,
        _generation: Option<u64>,
        reservation: uob_application::ProfileReservation201,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        self.dispatch_profile(command, Some(reservation))
    }
    fn dispatch(
        &self,
        command: Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        self.dispatch_profile(command, None)
    }
    fn device_model_expectation(
        &self,
        command: &Command<Value>,
        _generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_contracts::DeviceModelResult201>, CommandErrorCode> {
        self.device_expectation(command, now)
    }
    fn trigger_expectation(
        &self,
        command: &Command<Value>,
    ) -> Option<uob_application::TriggerExpectation> {
        if self.handle.is_closed() {
            return None;
        }
        let snapshot = self.snapshot.read().ok()?;
        mapping::trigger_expectation(command, &snapshot, self.clock.now())
    }
    fn context(
        &self,
        resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async move {
            let snapshot = self.snapshot.read().map_err(|_| state_error())?;
            Ok(
                mapping::capabilities(&snapshot, &resource).map(|capabilities| {
                    StationCommandContext {
                        connectivity: if self.handle.is_closed() {
                            Connectivity::Disconnected
                        } else {
                            snapshot.connectivity.clone()
                        },
                        capabilities: capabilities.clone(),
                    }
                }),
            )
        })
    }
}

fn state_error() -> StationCommandError {
    StationCommandError::new("remote control station state unavailable")
}
