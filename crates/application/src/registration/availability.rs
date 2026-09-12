//! Atomic status snapshots and journal evidence for availability reconciliation.
use super::{ConnectorStatusObservation, RegistrationError, status_snapshot};
use crate::{AtomicStoreWrite, OperationalStore};
use uob_contracts::{
    EventEnvelope, EventId, EventOrigin, EventType, ServiceIdentity, StationSnapshot, UtcTimestamp,
};

/// Trusted station-owner context; IDs and sequence come from the ordinary durable event stream.
pub struct AvailabilityContext {
    pub identity: ServiceIdentity,
    pub event_id: EventId,
    pub sequence: u64,
}

/// Commits a status observation and its complete bounded station evidence in one transaction.
/// The host uses this path when it needs durable availability reconciliation, in place of `status`.
/// It must serialize calls with other station updates and publish/reply only after success.
/// `E` permits the host's typed business-event enum to wrap the snapshot without raw wire JSON.
///
/// # Errors
/// Rejects invalid status/topology, mismatched trusted bridge identity, oversize evidence and
/// persistence failures without changing the caller's state or exposing an event before commit.
pub async fn record_status<C, E, D, R>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    observation: &ConnectorStatusObservation,
    context: AvailabilityContext,
    now: UtcTimestamp,
) -> Result<(), RegistrationError>
where
    C: Send + 'static,
    E: From<StationSnapshot> + Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    let next = status_snapshot(snapshot, observation, now)?;
    commit_status(store, snapshot, next, observation.source_time, context, now).await
}

/// Atomically records native OCPP 2.0.1 connector status and journal evidence.
/// # Errors
/// Rejects invalid registration, topology, identity, or persistence failure.
pub async fn record_status_201<C, E, D, R>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    observation: &super::v201::StatusObservation,
    context: AvailabilityContext,
    now: UtcTimestamp,
) -> Result<(), RegistrationError>
where
    C: Send + 'static,
    E: From<StationSnapshot> + Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    let next = super::v201::status_snapshot(snapshot, observation, now)?;
    commit_status(
        store,
        snapshot,
        next,
        Some(observation.source_time),
        context,
        now,
    )
    .await
}

async fn commit_status<C, E, D, R>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    next: StationSnapshot,
    source_time: Option<UtcTimestamp>,
    context: AvailabilityContext,
    now: UtcTimestamp,
) -> Result<(), RegistrationError>
where
    C: Send + 'static,
    E: From<StationSnapshot> + Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    if context.identity.bridge_id != snapshot.station.bridge_id || context.sequence == 0 {
        return Err(RegistrationError::InvalidState);
    }
    if serde_json::to_vec(&next)
        .map_err(|_| RegistrationError::InvalidState)?
        .len()
        > 256 * 1024
    {
        return Err(RegistrationError::InvalidState);
    }
    let event = EventEnvelope {
        event_id: context.event_id,
        schema_version: next.schema_version,
        runtime: context.identity.runtime,
        resource: next.station.clone(),
        source_time,
        observed_at: now,
        event_type: EventType::new("station.availability.observed")
            .map_err(|_| RegistrationError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: E::from(next.clone()),
    };
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    write.journal_events.push(event);
    store
        .write_atomic(write)
        .await
        .map_err(RegistrationError::Storage)?;
    *snapshot = next;
    Ok(())
}
