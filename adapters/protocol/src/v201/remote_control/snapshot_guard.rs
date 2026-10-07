//! Bounds and identity checks for the committed snapshot bound to one OCPP 2.0.1 socket.
use super::state_error;
use crate::CallSessionHandle;
use uob_application::StationCommandError;
use uob_contracts::{Connectivity, ProtocolEdition, StationSnapshot, UtcTimestamp};

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
