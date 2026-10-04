//! OCPP 2.0.1 remote operations behind the ordinary durable application command path.
use crate::remote_constraints as constraints;
mod charging_limit;
mod charging_profile201;
mod configuration201;
pub(crate) mod configuration201_profile_parse;
mod configuration201_response;
pub mod configuration201_values;
pub(crate) mod configuration201_wire;
pub mod device_model;
mod device_model_collection;
mod device_model_response;
pub mod device_model_values;
mod identity;
mod mapping;
mod phase_capability;
mod trigger;
pub use identity::LocalRemoteStartIdentity;
pub mod observation;

use crate::{CallSessionHandle, OutboundCall, PendingCall, SessionCallOutcome, SessionSubmitError};
use serde_json::Value;
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
    snapshot: RwLock<StationSnapshot>,
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
        Ok(Self {
            handle,
            snapshot: RwLock::new(snapshot),
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
    ) -> CommandDispatchOutcome {
        match pending.receive().await {
            SessionCallOutcome::Result { payload, .. } => {
                let outcome = if let Some(request) = profile {
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
    pub(super) fn dispatch_profile(
        &self,
        command: Command<Value>,
        reservation: Option<uob_application::ProfileReservation201>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
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
            let remote_start_id = if matches!(
                command.operation,
                uob_contracts::CommandOperation::Start { .. }
            ) {
                match self
                    .evidence
                    .reserve_remote_start(command.request_id.clone())
                    .await
                {
                    Ok(id) => Some(id),
                    Err(_) => return Ok(mapping::not_sent(CommandErrorCode::PolicyRejected)),
                }
            } else {
                None
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
                .receive_remote_response(&command.request_id, action, pending, profile)
                .await)
        })
    }
}

impl StationCommandPort<Value> for RemoteControlSession {
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
    if handle.protocol() != ProtocolEdition::Ocpp201
        || handle.station_id() != &snapshot.station.station_id
        || !matches!(
            snapshot.connectivity,
            Connectivity::Connected {
                protocol: ProtocolEdition::Ocpp201,
                ..
            }
        )
        || snapshot
            .resources
            .iter()
            .filter_map(|entry| crate::command_registry::charging_profile201::evse(&entry.resource))
            .filter(|evse| *evse > 0)
            .count()
            > 64
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
