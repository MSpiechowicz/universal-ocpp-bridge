//! One journal record per acknowledged `SecurityEventNotification`, committed atomically with
//! the station's activity before the CALL is answered. No data point or canonical state changes.
use super::SecurityEventObservation16;
use crate::{
    AtomicStoreWrite, ObservationCommitError, OperationalStore, StationEvent, registration,
    transaction16::TransactionContext,
};
use uob_contracts::{
    EventEnvelope, EventOrigin, EventType, StationSnapshot, TransactionSnapshot, UtcTimestamp,
};

/// Atomically commits one admitted event and the station's activity. `shed_calls` is the count
/// reported by [`super::SecurityEventDecision16::Store`]. The station owner holds its ordered
/// lock and answers the CALL only after this returns.
///
/// # Errors
///
/// Rejects an invalid context, a station without current accepted 1.6 registration, and failed
/// persistence. The snapshot is unchanged on error.
pub async fn record_security_event_16<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    observation: SecurityEventObservation16,
    shed_calls: u32,
    context: TransactionContext,
    now: UtcTimestamp,
) -> Result<(), ObservationCommitError> {
    if context.sequence == 0
        || context.identity.bridge_id != snapshot.station.bridge_id
        || context.identity.selected_target_id != context.target.as_ref().map(|(id, _)| id.clone())
    {
        return Err(ObservationCommitError::InvalidState);
    }
    registration::accepted(snapshot).map_err(|_| ObservationCommitError::InvalidState)?;

    let mut next = snapshot.clone();
    registration::activity(&mut next, now);

    let record = observation.into_record(shed_calls);
    let event = EventEnvelope {
        event_id: context.event_id,
        schema_version: next.schema_version,
        runtime: context.identity.runtime,
        resource: next.station.clone(),
        source_time: Some(record.timestamp),
        observed_at: now,
        event_type: EventType::new("station.security_event.16")
            .map_err(|_| ObservationCommitError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id,
        causation_id: None,
        provenance: None,
        payload: StationEvent::SecurityEvent16 {
            station_snapshot_invalidated: next.station.station_id.clone(),
            security_event_16: record,
        },
    };

    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    write.journal_events.push(event);
    store
        .write_atomic(write)
        .await
        .map_err(ObservationCommitError::Storage)?;

    *snapshot = next;
    Ok(())
}
