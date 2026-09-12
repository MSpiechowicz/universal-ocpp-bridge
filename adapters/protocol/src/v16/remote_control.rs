//! OCPP 1.6 remote operations behind the ordinary durable application command path.
use crate::remote_constraints as constraints;
mod identity;
mod mapping;
pub use identity::LocalRemoteStartIdentity;
pub mod observation;

use crate::{CallSessionHandle, OutboundCall, SessionCallOutcome, SessionSubmitError};
use serde_json::Value;
use std::sync::{Arc, RwLock};
use tokio::time::Instant;
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
    snapshot: RwLock<StationSnapshot>,
    identity: Arc<dyn RemoteStartIdentity>,
    clock: Arc<dyn CommandClock>,
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
    ) -> Result<Self, StationCommandError> {
        validate_snapshot(&handle, &snapshot)?;
        Ok(Self {
            handle,
            snapshot: RwLock::new(snapshot),
            identity,
            clock,
        })
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
        *current = snapshot;
        Ok(())
    }
}

impl StationCommandPort<Value> for RemoteControlSession {
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

    fn dispatch(
        &self,
        command: Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            // No await between checking committed state, resolving local policy, and bounded enqueue.
            let prepared = {
                let snapshot = self.snapshot.read().map_err(|_| state_error())?;
                if self.handle.is_closed() {
                    return Ok(mapping::not_sent(CommandErrorCode::StationDisconnected));
                }
                mapping::prepare(
                    &command,
                    &snapshot,
                    self.identity.as_ref(),
                    self.clock.now(),
                )
            };
            let (action, payload) = match prepared {
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
                        SessionSubmitError::InvalidRequest => CommandErrorCode::InvalidParameters,
                        SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
                            CommandErrorCode::PolicyRejected
                        }
                    }));
                }
            };
            Ok(match pending.receive().await {
                SessionCallOutcome::Result { payload, .. } => mapping::response(action, &payload),
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
