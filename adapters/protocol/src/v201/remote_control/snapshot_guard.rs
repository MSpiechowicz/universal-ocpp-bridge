//! Bounds and identity checks for the committed snapshot bound to one OCPP 2.0.1 socket.
use super::{RemoteControlSession, state_error};
use crate::{CallSessionHandle, OutboundCall, PendingCall, SessionSubmitError};
use serde_json::Value;
use uob_application::StationCommandError;
use uob_contracts::{
    Command, CommandErrorCode, Connectivity, ProtocolEdition, StationSnapshot, UtcTimestamp,
};

pub(super) fn connected_at(snapshot: &StationSnapshot) -> Option<UtcTimestamp> {
    match snapshot.connectivity {
        Connectivity::Connected { connected_at, .. } => Some(connected_at),
        _ => None,
    }
}
pub(super) fn validate_snapshot(
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

impl RemoteControlSession {
    /// Queues one privileged native call after provider work. Authority is rechecked under
    /// the snapshot lock right before the call is handed to the socket, and the call cannot
    /// outlive the command's own expiry.
    pub(super) fn enqueue_rechecked(
        &self,
        command: &Command<Value>,
        action: &'static str,
        payload: Value,
        recheck: impl FnOnce(&StationSnapshot, UtcTimestamp) -> Result<(), CommandErrorCode>,
    ) -> Result<PendingCall, CommandErrorCode> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let now = self.clock.now();
        recheck(&snapshot, now)?;
        if now >= command.expires_at {
            return Err(CommandErrorCode::Expired);
        }
        let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
        let deadline =
            tokio::time::Instant::now() + remaining.min(std::time::Duration::from_hours(24));
        let call = OutboundCall {
            message_id: command.request_id.as_str().to_owned(),
            action: uob_contracts::ProtocolActionName::new(action).expect("static action"),
            payload,
            correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                uob_contracts::CorrelationId::new(command.request_id.as_str())
                    .expect("request identity")
            }),
        };
        self.handle
            .try_call_before(call, deadline)
            .map_err(|error| match error {
                SessionSubmitError::Closed => CommandErrorCode::StationDisconnected,
                SessionSubmitError::InvalidRequest => CommandErrorCode::InvalidParameters,
                SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
                    CommandErrorCode::PolicyRejected
                }
            })
    }
}
