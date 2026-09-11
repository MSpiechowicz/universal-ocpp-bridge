//! Native OCPP 2.0.1 status and keepalive transitions.
use super::{RegistrationError, accepted_for, activity, commit, set};
use crate::OperationalStore;
use uob_contracts::{
    AvailabilityState, NativeProtocolReference, ProtocolEdition, StationSnapshot, TypedValue,
    UtcTimestamp,
};

/// `StatusNotification` addresses exactly one existing EVSE connector, never station zero.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusObservation {
    pub evse_id: u32,
    pub connector_id: u32,
    pub status: String,
    pub source_time: UtcTimestamp,
}

/// Commits server-observed heartbeat activity after accepted registration.
/// # Errors
/// Rejects disconnected/wrong-edition/unregistered stations and failed persistence.
pub async fn heartbeat<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    now: UtcTimestamp,
) -> Result<(), RegistrationError> {
    accepted_for(snapshot, ProtocolEdition::Ocpp201)?;
    let mut next = snapshot.clone();
    activity(&mut next, now);
    commit(store, snapshot, next).await
}

/// Persists native status without interpreting Occupied as physical charging or command success.
/// # Errors
/// Rejects invalid topology/status, unregistered stations and failed persistence.
pub async fn status<C: Send + 'static, E: Send + 'static, D: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    observation: &StatusObservation,
    now: UtcTimestamp,
) -> Result<(), RegistrationError> {
    accepted_for(snapshot, ProtocolEdition::Ocpp201)?;
    if observation.evse_id == 0 || observation.connector_id == 0 {
        return Err(RegistrationError::InvalidStatus);
    }
    let availability = match observation.status.as_str() {
        "Available" => AvailabilityState::Available,
        "Occupied" | "Reserved" => AvailabilityState::Occupied,
        "Unavailable" => AvailabilityState::Unavailable,
        "Faulted" => AvailabilityState::Faulted,
        _ => return Err(RegistrationError::InvalidStatus),
    };
    let mut next = snapshot.clone();
    let resource = next
        .resources
        .iter_mut()
        .find(|r| {
            r.resource.native_protocol_reference
                == Some(NativeProtocolReference::Ocpp201 {
                    evse_id: observation.evse_id,
                    connector_id: Some(observation.connector_id),
                })
        })
        .ok_or(RegistrationError::InvalidStatus)?;
    let id = format!(
        "ocpp201/evse-{}/connector-{}/status/status",
        observation.evse_id, observation.connector_id
    );
    let stale = resource
        .current_values
        .iter()
        .find(|v| v.point_id.as_str() == id)
        .is_some_and(|v| observation.source_time < v.source_time.unwrap_or(v.observed_at));
    if !stale {
        resource.availability = availability;
        set(
            &mut resource.current_values,
            &id,
            Some(TypedValue::Text(observation.status.clone())),
            Some(observation.source_time),
            now,
        );
    }
    activity(&mut next, now);
    commit(store, snapshot, next).await
}
