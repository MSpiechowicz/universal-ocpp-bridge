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
    record_transaction_event_with_trigger(store, snapshot, observation, context, now, None).await
}

/// Commits a 2.0.1 transaction and its compatible native trigger marker atomically.
/// # Errors
/// Rejects invalid registration, transaction, context, marker or failed persistence.
pub async fn record_transaction_event_with_trigger<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    observation: &TransactionEventObservation,
    context: TransactionContext,
    now: UtcTimestamp,
    trigger: Option<EventEnvelope<StationEvent>>,
) -> Result<TransactionApplyOutcome, ObservationCommitError> {
    record_transaction_event_with_reservation(
        store,
        snapshot,
        observation,
        context,
        now,
        trigger,
        None,
    )
    .await
}

/// Commits a 2.0.1 transaction, optional trigger marker and the station's native reservation
/// termination fact atomically. `reservation_group` is a trusted provider-derived group key.
/// # Errors
/// Rejects invalid registration, transaction, context, marker or failed persistence.
pub async fn record_transaction_event_with_reservation<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    observation: &TransactionEventObservation,
    context: TransactionContext,
    now: UtcTimestamp,
    trigger: Option<EventEnvelope<StationEvent>>,
    reservation_group: Option<crate::ReservationKey201>,
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
    if trigger.as_ref().is_some_and(|marker| {
        marker.resource != next.station
            || marker.sequence == event.sequence
            || marker.event_id == event.event_id
            || marker.sequence == invalidation.sequence
            || marker.event_id == invalidation.event_id
    }) {
        return Err(ObservationCommitError::InvalidState);
    }
    let mut write = AtomicStoreWrite::empty();
    write
        .reservation_observations_201
        .extend(reservation_observation(
            &next,
            observation,
            now,
            reservation_group,
        ));
    write.purpose = match transaction.state {
        TransactionState::Ended => StorageWritePurpose::ActiveSessionCompletion,
        _ if observation.event == TransactionEventKind::Started => {
            StorageWritePurpose::NewSessionStart
        }
        _ => StorageWritePurpose::Routine,
    };
    write.station_snapshot = Some(next.clone());
    write.journal_events.extend([event, invalidation]);
    if let Some(marker) = trigger {
        write.journal_events.push(marker);
    }
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
    record_measurements_with_trigger(store, snapshot, observation, context, now, None).await
}

/// Commits meter state plus optional typed trigger evidence as one durable operation.
/// # Errors
/// Rejects invalid registration, native resource, measurement, context or trigger marker, and
/// persistence failures without changing the caller's snapshot.
pub async fn record_measurements_with_trigger<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, StationEvent, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    observation: &MeasurementObservation,
    context: TransactionContext,
    now: UtcTimestamp,
    trigger: Option<EventEnvelope<StationEvent>>,
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
    if trigger.as_ref().is_some_and(|marker| {
        marker.resource != next.station
            || marker.sequence == event.sequence
            || marker.event_id == event.event_id
    }) {
        return Err(ObservationCommitError::InvalidState);
    }
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    write.journal_events.push(event);
    if let Some(marker) = trigger {
        write.journal_events.push(marker);
    }
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

/// The station's native termination fact for an OCPP 2.0.1 transaction, if it reports one.
fn reservation_observation(
    snapshot: &StationSnapshot,
    observation: &TransactionEventObservation,
    now: UtcTimestamp,
    reservation_group: Option<crate::ReservationKey201>,
) -> Option<crate::ReservationObservation201> {
    let (
        Some(reservation_id),
        NativeProtocolReference::Ocpp201 { evse_id, .. },
        ProtocolEdition::Ocpp201,
    ) = (
        observation.reservation_id,
        observation.native_resource,
        observation.protocol,
    )
    else {
        return None;
    };
    Some(crate::ReservationObservation201 {
        station: snapshot.station.clone(),
        observed_at: now,
        kind: crate::ReservationObservationKind201::Transaction {
            reservation_id,
            evse_id,
            token_key: observation.reservation_token_key.clone(),
            group_key: reservation_group,
            source_time: observation.occurred_at,
        },
    })
}
