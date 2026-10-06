//! OCPP 1.6 remote operations behind the ordinary durable application command path.
use crate::remote_constraints as constraints;
mod charging_limit;
mod charging_profile;
mod composite_schedule;
mod configuration;
mod configuration_values;
mod enqueue;
pub(crate) mod exact_rate;
mod identity;
mod local_authorization;
mod local_authorization_values;
mod mapping;
mod reservation;
mod reservation_values;
mod trigger;
pub(crate) use configuration_values::DeferredConfigurationCall;
pub use configuration_values::{
    LocalConfigurationValues, ProtectedConfigurationText, ProtectedConfigurationValue,
};
pub use identity::LocalRemoteStartIdentity;
pub(crate) use local_authorization::DeferredLocalAuthorizationCall16;
pub use local_authorization_values::{
    LocalAuthorizationUpdates16, ProtectedLocalListUpdate16, ProtectedLocalListValue16,
    wipe_local_authorization_json,
};
pub(crate) use reservation::DeferredReservationCall16;
pub use reservation::ReservationGrant16;
pub use reservation::response as reservation_response_16;
pub use reservation_values::{ReservationValues16, reservation_key_16};
pub mod observation;

use crate::{CallSessionHandle, SessionCallOutcome};
use serde_json::Value;
use std::sync::{Arc, RwLock};
use uob_application::{
    CommandClock, CommandDispatchOutcome, SensitiveAuthorizationToken, StationCommandContext,
    StationCommandError, StationCommandFuture, StationCommandPort,
};
use uob_contracts::{
    Command, CommandErrorCode, Connectivity, ProtocolEdition, ResourceRef, StationSnapshot,
    UtcTimestamp,
};

/// Narrow trusted provider for a remote start's opaque authorization reference.
/// Implementations must resolve only locally configured references, recheck current local policy
/// for the exact resource/time, and return no token on revocation, expiry or provider failure.
/// This synchronous port must be bounded and must not perform network or blocking I/O.
/// Raw idTags are exposed only while constructing the socket payload, never persisted in commands.
pub trait RemoteStartIdentity: Send + Sync {
    fn authorized_token(
        &self,
        reference: &str,
        resource: &ResourceRef,
        now: UtcTimestamp,
    ) -> Option<SensitiveAuthorizationToken>;
}

/// One admitted OCPP 1.6 socket and its latest committed snapshot. Construct a new port for a
/// reconnect; never replace its socket handle. The host's station registry routes exact resources
/// here and wraps its coordinator with scoped access and charging authorization guards.
pub struct RemoteControlSession {
    handle: CallSessionHandle,
    snapshot: Arc<RwLock<StationSnapshot>>,
    identity: Arc<dyn RemoteStartIdentity>,
    configuration_values: Option<Arc<LocalConfigurationValues>>,
    configuration_facts: RwLock<configuration::SessionFacts>,
    local_authorization_updates: Option<Arc<LocalAuthorizationUpdates16>>,
    local_list_limits: Arc<std::sync::Mutex<local_authorization::LocalListLimits16>>,
    local_list_active: Arc<std::sync::Mutex<bool>>,
    reservation_values: Option<Arc<ReservationValues16>>,
    reservation_grant: Option<Arc<ReservationGrant16>>,
    reserve_zero: bool,
    reservation_active: Arc<std::sync::Mutex<bool>>,
    clock: Arc<dyn CommandClock>,
    evidence: Arc<dyn uob_application::remote_control::RemoteControlStore>,
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
        Ok(Self {
            handle,
            snapshot: Arc::new(RwLock::new(snapshot)),
            identity,
            clock,
            configuration_values: None,
            configuration_facts: RwLock::new(configuration::SessionFacts::default()),
            local_authorization_updates: None,
            local_list_limits: Arc::new(std::sync::Mutex::new(
                local_authorization::LocalListLimits16::default(),
            )),
            local_list_active: Arc::new(std::sync::Mutex::new(true)),
            evidence,
            reservation_values: None,
            reservation_grant: None,
            reserve_zero: false,
            reservation_active: Arc::new(std::sync::Mutex::new(true)),
        })
    }
    /// Installs locally provisioned station/key-bound values for this authenticated socket only.
    #[must_use]
    pub fn with_configuration_values(mut self, values: Arc<LocalConfigurationValues>) -> Self {
        self.configuration_values = Some(values);
        self
    }

    #[must_use]
    pub fn with_local_authorization_updates(
        mut self,
        provider: Arc<LocalAuthorizationUpdates16>,
    ) -> Self {
        self.local_authorization_updates = Some(provider);
        self
    }
    #[must_use]
    pub fn with_reservations_16(
        mut self,
        provider: Option<Arc<ReservationValues16>>,
        reserve_zero: bool,
        grant: Arc<ReservationGrant16>,
    ) -> Self {
        self.reservation_values = provider;
        self.reserve_zero = reserve_zero;
        self.reservation_grant = Some(grant);
        self
    }

    /// Retires this exact socket generation before any queued protected send can begin.
    pub fn detach_local_authorization(&self) {
        if let Ok(mut active) = self.local_list_active.lock() {
            *active = false;
        }
        if let Ok(mut active) = self.reservation_active.lock() {
            *active = false;
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
        let enabled = snapshot.capabilities.operations.iter().any(|supported| {
            matches!(&supported.operation, uob_contracts::Operation::ProtocolAction { protocol: ProtocolEdition::Ocpp16j, action } if action == "SendLocalList")
        });
        if !enabled {
            *self.local_list_active.lock().map_err(|_| state_error())? = false;
        }
        *current = snapshot;
        Ok(())
    }

    fn configuration_response(
        &self,
        action: &str,
        payload: &Value,
        command: &Command<Value>,
    ) -> Result<CommandDispatchOutcome, StationCommandError> {
        let uob_contracts::CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(mapping::uncertain());
        };
        let configuration = if action == "GetConfiguration" {
            configuration::read_response(payload, &operation.payload)
        } else {
            operation.payload["key"]
                .as_str()
                .and_then(|key| configuration::write_response(payload, key))
        };
        let Some(configuration) = configuration else {
            return Ok(mapping::uncertain());
        };
        if action == "GetConfiguration" {
            self.configuration_facts
                .write()
                .map_err(|_| state_error())?
                .learn(&configuration);
            self.local_list_limits
                .lock()
                .map_err(|_| state_error())?
                .learn(&configuration);
        }
        let (accepted, error) = match &configuration {
            uob_contracts::ConfigurationResult::Read { .. } => (true, None),
            uob_contracts::ConfigurationResult::Write { status, .. } => {
                let accepted = matches!(
                    status,
                    uob_contracts::ConfigurationWriteStatus::Accepted
                        | uob_contracts::ConfigurationWriteStatus::RebootRequired
                );
                let error = (!accepted).then(|| uob_contracts::CommandError {
                    code: CommandErrorCode::ProtocolRejected,
                    detail: Some(format!("{status:?}")),
                });
                (accepted, error)
            }
        };
        Ok(CommandDispatchOutcome::ConfigurationResponse {
            accepted,
            error,
            configuration,
        })
    }
}

impl StationCommandPort<Value> for RemoteControlSession {
    fn reservation_expectation_16(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_application::ReservationMutation16>, CommandErrorCode> {
        self.reservation_context(command, generation, now)
    }
    fn charging_profile_expectation(
        &self,
        command: &Command<Value>,
        _generation: Option<u64>,
        _now: UtcTimestamp,
    ) -> Result<Option<uob_application::ProfileReservation201>, CommandErrorCode> {
        if matches!(&command.operation, uob_contracts::CommandOperation::Ocpp(operation)
            if operation.protocol == uob_contracts::ProtocolEdition::Ocpp201)
        {
            return Err(CommandErrorCode::UnsupportedOperation);
        }
        Ok(None)
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

    #[allow(clippy::too_many_lines)] // Routes every native 1.6 action through one dispatch gate.
    fn dispatch(
        &self,
        command: Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            let (action, unqueued_payload, profile_request, ready_pending) = {
                let snapshot = self.snapshot.read().map_err(|_| state_error())?;
                let mut profile_request = None;
                if self.handle.is_closed() {
                    return Ok(mapping::not_sent(CommandErrorCode::StationDisconnected));
                }
                let facts = self.configuration_facts.read().map_err(|_| state_error())?;
                let (action, payload) = match mapping::prepare(
                    &command,
                    &snapshot,
                    self.identity.as_ref(),
                    self.configuration_values.as_deref(),
                    &facts,
                    self.clock.now(),
                    &mut profile_request,
                ) {
                    Ok(prepared) => prepared,
                    Err(code) => return Ok(mapping::not_sent(code)),
                };

                // Full native profiles retain the same snapshot guard through bounded
                // enqueue, so an ending transaction or revoked capability cannot commit
                // between validation and queue admission. Never retain guards for reply wait.
                let (unqueued_payload, ready_pending) = if profile_request.is_some() {
                    let pending =
                        match self.enqueue_prepared(&command, action, payload, self.clock.now()) {
                            Ok(pending) => pending,
                            Err(code) => return Ok(mapping::not_sent(code)),
                        };
                    (None, Some(pending))
                } else {
                    (Some(payload), None)
                };
                (action, unqueued_payload, profile_request, ready_pending)
            };

            let (pending, schedule_request) = if let Some(pending) = ready_pending {
                (pending, None)
            } else {
                // Preserve the existing canonical/configuration/composite preparation order.
                let now = self.clock.now();
                let schedule_request = match composite_schedule::request_context(&command, action) {
                    Ok(request) => request,
                    Err(code) => return Ok(mapping::not_sent(code)),
                };
                let pending = match self.enqueue_prepared(
                    &command,
                    action,
                    unqueued_payload.expect("unqueued ordinary request"),
                    now,
                ) {
                    Ok(pending) => pending,
                    Err(code) => return Ok(mapping::not_sent(code)),
                };
                (pending, schedule_request)
            };
            Ok(match pending.receive().await {
                SessionCallOutcome::Result { mut payload, .. } => {
                    if crate::command_registry::reservation16::ACTIONS.contains(&action) {
                        return Ok(reservation::response(
                            action,
                            &payload,
                            &command,
                            self.clock.now(),
                        ));
                    }
                    if crate::command_registry::local_authorization16::ACTIONS.contains(&action) {
                        let outcome = local_authorization::response(action, &payload, &command);
                        wipe_local_authorization_json(&mut payload);
                        return Ok(outcome);
                    }
                    if action == "GetConfiguration" || action == "ChangeConfiguration" {
                        return self.configuration_response(action, &payload, &command);
                    }
                    if let Some(request) = schedule_request {
                        return Ok(composite_schedule::response(request, &payload));
                    }
                    if let Some(request) = profile_request {
                        return Ok(charging_profile::response(request, &payload));
                    }
                    let outcome = mapping::response(action, &payload);
                    if action == "ChangeAvailability"
                        && matches!(outcome, CommandDispatchOutcome::ProtocolResponse { .. })
                        && self
                            .evidence
                            .record_remote_response(
                                command.request_id.clone(),
                                payload["status"]
                                    .as_str()
                                    .expect("validated status")
                                    .to_owned(),
                                None,
                            )
                            .await
                            .is_err()
                    {
                        return Ok(mapping::uncertain());
                    }
                    outcome
                }
                SessionCallOutcome::Error { .. }
                    if crate::command_registry::reservation16::ACTIONS.contains(&action) =>
                {
                    mapping::uncertain()
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
            })
        })
    }
}

fn connected_at(snapshot: &StationSnapshot) -> Option<UtcTimestamp> {
    match snapshot.connectivity {
        Connectivity::Connected { connected_at, .. } => Some(connected_at),
        _ => None,
    }
}
fn validate_snapshot(
    handle: &CallSessionHandle,
    snapshot: &StationSnapshot,
) -> Result<(), StationCommandError> {
    if handle.protocol() != ProtocolEdition::Ocpp16j
        || handle.station_id() != &snapshot.station.station_id
        || !matches!(
            snapshot.connectivity,
            Connectivity::Connected {
                protocol: ProtocolEdition::Ocpp16j,
                ..
            }
        )
        || serde_json::to_vec(snapshot)
            .map_err(|_| state_error())?
            .len()
            > 256 * 1024
    {
        return Err(state_error());
    }
    Ok(())
}
fn state_error() -> StationCommandError {
    StationCommandError::new("remote control station state unavailable")
}
