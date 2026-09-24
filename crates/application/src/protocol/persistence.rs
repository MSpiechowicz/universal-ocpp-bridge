//! Ordered station-owner commits for OCPP transaction and meter observations.
use super::{
    MeasurementApplyError, MeasurementObservation, TransactionApplyError, TransactionApplyOutcome,
    TransactionEventKind, TransactionEventObservation, apply_measurements, apply_transaction_event,
};
use crate::{
    AtomicStoreWrite, DeliveryId, Durability, OperationalStore, PendingDelivery, StationEvent,
    StorageError, StorageWritePurpose, registration, transaction16::TransactionContext,
};
use uob_contracts::{
    EventEnvelope, EventId, EventOrigin, EventType, NativeProtocolReference, ProtocolEdition,
    StationSnapshot, TransactionSnapshot, TransactionState, UtcTimestamp,
};

#[derive(Debug)]
pub enum ObservationCommitError {
    InvalidState,
    Transaction(TransactionApplyError),
    Measurement(MeasurementApplyError),
    Storage(StorageError),
}

/// Atomically commits a non-duplicate lifecycle event, its transaction delivery, and a
/// station-scoped snapshot invalidation. The station owner holds its ordered lock across commit.
/// A repeated OCPP sequence with identical protocol evidence never emits another event.
/// # Errors
/// Rejects invalid context, registration, ordering, resource identity or failed persistence.
pub async fn record_transaction_event<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    observation: &TransactionEventObservation,
    context: TransactionContext,
    now: UtcTimestamp,
) -> Result<TransactionApplyOutcome, ObservationCommitError> {
    validate_context(snapshot, &context)?;
    registration::v201::accepted(snapshot).map_err(|_| ObservationCommitError::InvalidState)?;
    let mut next = snapshot.clone();
    let outcome = apply_transaction_event(&mut next, observation, now)
        .map_err(ObservationCommitError::Transaction)?;
    if outcome == TransactionApplyOutcome::Duplicate {
        return Ok(outcome);
    }
    let transaction = next
        .transactions
        .iter()
        .find(|transaction| {
            transaction.protocol_state.as_ref().is_some_and(|state| {
                state.protocol == ProtocolEdition::Ocpp201
                    && state.native_transaction_id == observation.native_transaction_id
            })
        })
        .ok_or(ObservationCommitError::InvalidState)?
        .clone();
    registration::activity(&mut next, now);
    let event_type = match observation.event {
        TransactionEventKind::Started => "transaction.started",
        TransactionEventKind::Updated => "transaction.updated",
        TransactionEventKind::Ended => "transaction.ended",
    };
    let event = EventEnvelope {
        event_id: context.event_id.clone(),
        schema_version: next.schema_version,
        runtime: context.identity.runtime.clone(),
        resource: transaction.resource.clone(),
        source_time: Some(observation.occurred_at),
        observed_at: now,
        event_type: EventType::new(event_type).map_err(|_| ObservationCommitError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id.clone(),
        causation_id: None,
        provenance: None,
        payload: StationEvent::Transaction(transaction.clone()),
    };
    let invalidation = station_invalidation(&next, &context, Some(observation.occurred_at), now)?;
    let mut write = AtomicStoreWrite::empty();
    write.purpose = match transaction.state {
        TransactionState::Ended => StorageWritePurpose::ActiveSessionCompletion,
        _ if observation.event == TransactionEventKind::Started => {
            StorageWritePurpose::NewSessionStart
        }
        _ => StorageWritePurpose::Routine,
    };
    write.station_snapshot = Some(next.clone());
    write.journal_events.extend([event, invalidation]);
    if let Some((target, revision)) = context.target {
        write.required_deliveries.push(PendingDelivery {
            delivery_id: DeliveryId::new(format!("transaction/{}", context.event_id.as_str()))
                .map_err(ObservationCommitError::Storage)?,
            event_id: context.event_id,
            target_instance_id: target,
            target_configuration_revision: revision,
            ordering_key: transaction.resource.clone(),
            deadline: context.delivery_deadline,
            durability: Durability::Critical,
            payload: transaction,
        });
    }
    store
        .write_atomic(write)
        .await
        .map_err(ObservationCommitError::Storage)?;
    *snapshot = next;
    Ok(outcome)
}

/// Commits canonical meter values and a station-scoped retained event after validation.
/// Standalone meter reports have no OCPP sequence; old source timestamps cannot rewind points.
/// # Errors
/// Rejects invalid registration, native resource, context or failed persistence.
pub async fn record_measurements<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    observation: &MeasurementObservation,
    context: TransactionContext,
    now: UtcTimestamp,
) -> Result<(), ObservationCommitError> {
    validate_context(snapshot, &context)?;
    match (observation.protocol, observation.native_resource) {
        (ProtocolEdition::Ocpp16j, NativeProtocolReference::Ocpp16 { .. }) => {
            registration::accepted(snapshot).map_err(|_| ObservationCommitError::InvalidState)?;
        }
        (ProtocolEdition::Ocpp201, NativeProtocolReference::Ocpp201 { .. }) => {
            registration::v201::accepted(snapshot)
                .map_err(|_| ObservationCommitError::InvalidState)?;
        }
        _ => return Err(ObservationCommitError::InvalidState),
    }
    let mut next = snapshot.clone();
    apply_measurements(&mut next, observation, now).map_err(ObservationCommitError::Measurement)?;
    registration::activity(&mut next, now);
    let event = EventEnvelope {
        event_id: context.event_id,
        schema_version: next.schema_version,
        runtime: context.identity.runtime,
        resource: next.station.clone(),
        source_time: observation
            .values
            .iter()
            .filter_map(|value| value.source_time)
            .max(),
        observed_at: now,
        event_type: EventType::new("station.measurements.observed")
            .map_err(|_| ObservationCommitError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id,
        causation_id: None,
        provenance: None,
        payload: StationEvent::StationSnapshot(next.clone()),
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

fn validate_context(
    snapshot: &StationSnapshot,
    context: &TransactionContext,
) -> Result<(), ObservationCommitError> {
    if context.sequence == 0
        || context.identity.bridge_id != snapshot.station.bridge_id
        || context.identity.selected_target_id != context.target.as_ref().map(|(id, _)| id.clone())
    {
        return Err(ObservationCommitError::InvalidState);
    }
    Ok(())
}

fn station_invalidation(
    snapshot: &StationSnapshot,
    context: &TransactionContext,
    source_time: Option<UtcTimestamp>,
    now: UtcTimestamp,
) -> Result<EventEnvelope<StationEvent>, ObservationCommitError> {
    Ok(EventEnvelope {
        event_id: EventId::new(format!("{}/station", context.event_id.as_str()))
            .map_err(|_| ObservationCommitError::InvalidState)?,
        schema_version: snapshot.schema_version,
        runtime: context.identity.runtime.clone(),
        resource: snapshot.station.clone(),
        source_time,
        observed_at: now,
        event_type: EventType::new("station.snapshot.changed")
            .map_err(|_| ObservationCommitError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id.clone(),
        causation_id: Some(context.event_id.clone()),
        provenance: None,
        payload: StationEvent::StationSnapshot(snapshot.clone()),
    })
}
