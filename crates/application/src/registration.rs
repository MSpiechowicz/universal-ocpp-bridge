//! Persisted registration and status transitions owned by the station's ordered task.
pub mod availability;
pub mod v201;
use crate::{AtomicStoreWrite, OperationalStore, RegistrationObservation, StorageError};
use uob_contracts::{
    AvailabilityState, Connectivity, DataPointValue, Freshness, NativeProtocolReference, PointId,
    ProtocolEdition, Quality, QualityLevel, StationSnapshot, TypedValue, UtcTimestamp,
};

/// Explicit administrator/policy decision, independent of transport authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationDecision {
    Accepted,
    Pending,
    Rejected,
}
impl RegistrationDecision {
    /// Exact OCPP registration status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Pending => "Pending",
            Self::Rejected => "Rejected",
        }
    }
}

/// Version-preserving status facts; adapters validate protocol values and field bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectorStatusObservation {
    pub connector_id: u32,
    pub status: String,
    pub error_code: String,
    pub info: Option<String>,
    pub vendor_id: Option<String>,
    pub vendor_error_code: Option<String>,
    pub source_time: Option<UtcTimestamp>,
}

/// Application lifecycle failure, deliberately free of external payload text.
#[derive(Debug)]
pub enum RegistrationError {
    InvalidState,
    NotRegistered,
    InvalidStatus,
    Storage(StorageError),
}

/// Applies one validated boot decision without discarding transactions or connector topology.
/// Success is returned only after the authoritative snapshot is committed.
///
/// # Errors
/// Rejects wrong protocol, disconnected transport, zero interval or failed storage.
pub async fn register<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    observation: &RegistrationObservation,
    decision: RegistrationDecision,
    interval_seconds: u32,
    now: UtcTimestamp,
) -> Result<RegistrationDecision, RegistrationError> {
    connected_for(snapshot, observation.protocol)?;
    if interval_seconds == 0 {
        return Err(RegistrationError::InvalidState);
    }
    let mut next = snapshot.clone();
    for (name, value) in [
        ("status", TypedValue::Text(decision.as_str().to_owned())),
        (
            "interval_seconds",
            TypedValue::UnsignedInteger(u64::from(interval_seconds)),
        ),
        ("vendor", TypedValue::Text(observation.vendor.clone())),
        ("model", TypedValue::Text(observation.model.clone())),
    ] {
        set(
            &mut next.current_values,
            &format!("{}/registration/{name}", namespace(observation.protocol)),
            Some(value),
            None,
            now,
        );
    }
    if observation.protocol == ProtocolEdition::Ocpp201 {
        set(
            &mut next.current_values,
            "ocpp201/registration/boot_reason",
            observation.boot_reason.clone().map(TypedValue::Text),
            None,
            now,
        );
    }
    activity(&mut next, now);
    commit(store, snapshot, next).await?;
    Ok(decision)
}

/// Records a heartbeat only for a previously accepted registration on an admitted transport.
///
/// # Errors
/// Returns lifecycle or persistence failure without mutating the caller's snapshot.
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
    accepted(snapshot)?;
    let mut next = snapshot.clone();
    activity(&mut next, now);
    commit(store, snapshot, next).await
}

/// Stores exact connector status/error facts and a coarse canonical availability projection.
/// Connector zero addresses the controller; it never creates an EVSE or charging connector.
/// Older device observations are acknowledged but do not rewind current resource state.
///
/// # Errors
/// Returns lifecycle, unknown connector, invalid status, or persistence failure.
pub async fn status<C: Send + 'static, E: Send + 'static, D: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    observation: &ConnectorStatusObservation,
    now: UtcTimestamp,
) -> Result<(), RegistrationError> {
    let next = status_snapshot(snapshot, observation, now)?;
    commit(store, snapshot, next).await
}

fn status_snapshot(
    snapshot: &StationSnapshot,
    observation: &ConnectorStatusObservation,
    now: UtcTimestamp,
) -> Result<StationSnapshot, RegistrationError> {
    accepted(snapshot)?;
    let availability = match observation.status.as_str() {
        "Available" => AvailabilityState::Available,
        "Unavailable" => AvailabilityState::Unavailable,
        "Faulted" => AvailabilityState::Faulted,
        "Preparing" | "Charging" | "SuspendedEVSE" | "SuspendedEV" | "Finishing" | "Reserved"
            if observation.connector_id != 0 =>
        {
            AvailabilityState::Occupied
        }
        _ => return Err(RegistrationError::InvalidStatus),
    };
    let mut next = snapshot.clone();
    let (values, resource_availability) = if observation.connector_id == 0 {
        (&mut next.current_values, None)
    } else {
        let resource = next
            .resources
            .iter_mut()
            .find(|r| {
                r.resource.native_protocol_reference
                    == Some(NativeProtocolReference::Ocpp16 {
                        connector_id: observation.connector_id,
                    })
            })
            .ok_or(RegistrationError::InvalidStatus)?;
        (
            &mut resource.current_values,
            Some(&mut resource.availability),
        )
    };
    let prefix = format!("ocpp16/connector-{}/status/", observation.connector_id);
    let previous = values
        .iter()
        .find(|v| v.point_id.as_str() == format!("{prefix}status"));
    let effective = observation.source_time.unwrap_or(now);
    let stale = previous.is_some_and(|v| effective < v.source_time.unwrap_or(v.observed_at));
    if !stale {
        if let Some(current) = resource_availability {
            *current = availability;
        }
        for (name, value) in [
            ("status", Some(observation.status.clone())),
            ("error_code", Some(observation.error_code.clone())),
            ("info", observation.info.clone()),
            ("vendor_id", observation.vendor_id.clone()),
            ("vendor_error_code", observation.vendor_error_code.clone()),
        ] {
            set(
                values,
                &format!("{prefix}{name}"),
                value.map(TypedValue::Text),
                observation.source_time,
                now,
            );
        }
    }
    activity(&mut next, now);
    Ok(next)
}

fn namespace(protocol: ProtocolEdition) -> &'static str {
    match protocol {
        ProtocolEdition::Ocpp16j => "ocpp16",
        ProtocolEdition::Ocpp201 => "ocpp201",
    }
}
fn connected_for(
    snapshot: &StationSnapshot,
    edition: ProtocolEdition,
) -> Result<(), RegistrationError> {
    if matches!(
        snapshot.connectivity,
        Connectivity::Connected {
            protocol,
            ..
        } if protocol == edition
    ) {
        Ok(())
    } else {
        Err(RegistrationError::InvalidState)
    }
}
/// Requires the station to have a currently accepted registration.
/// # Errors
/// Rejects disconnected, pending, rejected or unregistered station state.
pub fn accepted(snapshot: &StationSnapshot) -> Result<(), RegistrationError> {
    accepted_for(snapshot, ProtocolEdition::Ocpp16j)
}
fn accepted_for(
    snapshot: &StationSnapshot,
    edition: ProtocolEdition,
) -> Result<(), RegistrationError> {
    connected_for(snapshot, edition)?;
    if snapshot.current_values.iter().any(|v| {
        v.point_id.as_str() == format!("{}/registration/status", namespace(edition))
            && v.value == Some(TypedValue::Text("Accepted".to_owned()))
    }) {
        Ok(())
    } else {
        Err(RegistrationError::NotRegistered)
    }
}
pub(crate) fn activity(snapshot: &mut StationSnapshot, now: UtcTimestamp) {
    snapshot.observed_at = now;
    if let Connectivity::Connected {
        last_message_at, ..
    } = &mut snapshot.connectivity
    {
        *last_message_at = Some(now);
    }
}
pub(crate) fn set(
    values: &mut Vec<DataPointValue>,
    id: &str,
    value: Option<TypedValue>,
    source_time: Option<UtcTimestamp>,
    observed_at: UtcTimestamp,
) {
    let point = DataPointValue {
        point_id: PointId::new(id).expect("fixed point namespace"),
        value,
        source_time,
        observed_at,
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    };
    if let Some(existing) = values.iter_mut().find(|v| v.point_id == point.point_id) {
        *existing = point;
    } else {
        values.push(point);
    }
}
async fn commit<C: Send + 'static, E: Send + 'static, D: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, E, D, R>,
    current: &mut StationSnapshot,
    next: StationSnapshot,
) -> Result<(), RegistrationError> {
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    store
        .write_atomic(write)
        .await
        .map_err(RegistrationError::Storage)?;
    *current = next;
    Ok(())
}
