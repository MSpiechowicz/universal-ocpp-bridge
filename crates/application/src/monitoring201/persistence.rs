//! One bounded journal record per acknowledged `NotifyEvent` CALL, committed atomically with the
//! station's activity before the CALL is answered. No data point or canonical state changes.
use crate::{
    AtomicStoreWrite, ObservationCommitError, OperationalStore, StationEvent, registration,
    transaction16::TransactionContext,
};
use uob_contracts::{
    EventEnvelope, EventOrigin, EventType, MonitoringEventRecord201, StationSnapshot,
    TransactionSnapshot, UtcTimestamp,
};

/// Atomically commits one record from [`super::MonitoringEventGate201`] and the station's
/// activity. The station owner holds its ordered lock and answers the CALL only after this
/// returns.
///
/// # Errors
///
/// Rejects an invalid context, a station without current accepted 2.0.1 registration, and failed
/// persistence. The snapshot is unchanged on error.
pub async fn record_monitoring_events_201<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    record: MonitoringEventRecord201,
    context: TransactionContext,
    now: UtcTimestamp,
) -> Result<(), ObservationCommitError> {
    if context.sequence == 0
        || context.identity.bridge_id != snapshot.station.bridge_id
        || context.identity.selected_target_id != context.target.as_ref().map(|(id, _)| id.clone())
    {
        return Err(ObservationCommitError::InvalidState);
    }
    registration::v201::accepted(snapshot).map_err(|_| ObservationCommitError::InvalidState)?;

    let mut next = snapshot.clone();
    registration::activity(&mut next, now);

    let event = EventEnvelope {
        event_id: context.event_id,
        schema_version: next.schema_version,
        runtime: context.identity.runtime,
        resource: next.station.clone(),
        source_time: Some(record.generated_at),
        observed_at: now,
        event_type: EventType::new("station.monitoring_event.201")
            .map_err(|_| ObservationCommitError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id,
        causation_id: None,
        provenance: None,
        payload: StationEvent::MonitoringEvents201 {
            station_snapshot_invalidated: next.station.station_id.clone(),
            monitoring_events_201: record,
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
