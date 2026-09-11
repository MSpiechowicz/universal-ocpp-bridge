//! OCPP 1.6 lifecycle owned by the authenticated station's single ordered state owner.
mod retention;
use crate::{
    AtomicStoreWrite, DeliveryId, Durability, OperationalStore, PendingDelivery, StorageError,
    StorageWritePurpose, TransactionStartObservation, registration,
};
use uob_contracts::{
    CorrelationId, DataPointValue, EventEnvelope, EventId, EventOrigin, EventType,
    NativeProtocolReference, Ocpp16TransactionEvidence, PointId, ServiceIdentity, StationSnapshot,
    TargetInstanceId, TransactionId, TransactionSnapshot, TransactionState, UtcTimestamp,
};

/// Hard bound on retained retry evidence. Exhaustion refuses starts; stops remain available.
/// No eviction may silently turn an old retry into a new charging transaction.
pub const MAX_RETAINED_TRANSACTIONS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StopObservation {
    pub transaction_id: i32,
    pub occurred_at: UtcTimestamp,
    pub meter_stop: i32,
    pub reason: Option<String>,
    pub identity_fingerprint: Option<String>,
    pub fingerprint: String,
    pub values: Vec<DataPointValue>,
    pub signed_values: Vec<String>,
}

#[derive(Debug)]
pub enum TransactionError {
    InvalidState,
    Conflict,
    Capacity,
    Storage(StorageError),
}

/// Trusted durable event identity/sequence supplied by the station's event owner.
/// Target identity and immutable revision come only from the selected configuration.
pub struct TransactionContext {
    pub identity: ServiceIdentity,
    pub event_id: EventId,
    pub sequence: u64,
    pub correlation_id: Option<CorrelationId>,
    pub target: Option<(TargetInstanceId, u64)>,
    pub delivery_deadline: UtcTimestamp,
}

/// Recognizes exact retries even if the charger uses a new CALL ID on reconnect.
/// # Errors
/// Rejects altered reuse of a retained CALL ID, unknown connectors, and unregistered stations.
pub fn replay_start(
    snapshot: &StationSnapshot,
    observation: &TransactionStartObservation,
    message_id: &str,
    now: UtcTimestamp,
) -> Result<Option<TransactionSnapshot>, TransactionError> {
    registration::accepted(snapshot).map_err(|_| TransactionError::InvalidState)?;
    if !matches!(observation.native_resource, NativeProtocolReference::Ocpp16 { connector_id } if connector_id > 0)
        || !snapshot
            .resources
            .iter()
            .any(|r| r.resource.native_protocol_reference == Some(observation.native_resource))
    {
        return Err(TransactionError::InvalidState);
    }
    check_message_id(snapshot, message_id, &observation.fingerprint, false)?;
    for transaction in &snapshot.transactions {
        if transaction
            .ocpp16
            .as_ref()
            .is_some_and(|state| state.start_fingerprint == observation.fingerprint)
        {
            return Ok(Some(transaction.clone()));
        }
    }
    if observation.occurred_at.into_inner().unix_timestamp() <= retention::floor(snapshot, now) {
        return Err(TransactionError::Conflict);
    }
    Ok(None)
}

/// Prepares a reported transaction independently of whether its authorization was accepted.
/// # Errors
/// Refuses capacity exhaustion, overlapping transactions, and invalid allocated identities.
#[allow(clippy::too_many_arguments)]
pub fn start(
    snapshot: &StationSnapshot,
    observation: &TransactionStartObservation,
    message_id: String,
    transaction_id: i32,
    status: String,
    expiry: Option<UtcTimestamp>,
    reference: Option<String>,
    now: UtcTimestamp,
) -> Result<TransactionSnapshot, TransactionError> {
    if transaction_id <= 0 || retention::count(snapshot, now) >= MAX_RETAINED_TRANSACTIONS {
        return Err(TransactionError::Capacity);
    }
    let resource = snapshot
        .resources
        .iter()
        .find(|r| r.resource.native_protocol_reference == Some(observation.native_resource))
        .ok_or(TransactionError::InvalidState)?
        .resource
        .clone();
    if snapshot
        .transactions
        .iter()
        .any(|t| t.resource == resource && t.state != TransactionState::Ended)
    {
        return Err(TransactionError::Conflict);
    }
    Ok(TransactionSnapshot {
        transaction_id: TransactionId::new(format!("ocpp16/{transaction_id}"))
            .map_err(|_| TransactionError::InvalidState)?,
        resource,
        // StartTransaction reports a transaction, not proof of power flow.
        state: TransactionState::Pending,
        started_at: observation.occurred_at,
        ended_at: None,
        protocol_state: None,
        ocpp16: Some(Box::new(Ocpp16TransactionEvidence {
            transaction_id,
            start_message_id: message_id,
            start_fingerprint: observation.fingerprint.clone(),
            authorization_status: status,
            authorization_expiry: expiry,
            identity_reference: reference,
            meter_start: observation.meter_start,
            reservation_id: observation.reservation_id,
            stop_message_id: None,
            stop_fingerprint: None,
            stop_identity_fingerprint: None,
            meter_stop: None,
            stop_reason: None,
            transaction_data: Vec::new(),
            signed_values: Vec::new(),
        })),
    })
}

/// Prepares a stop or recognizes a retry; a stop never requires renewed charging permission.
/// # Errors
/// Rejects unknown transactions, altered retries, and source times before the start.
pub fn stop(
    snapshot: &StationSnapshot,
    observation: &StopObservation,
    message_id: &str,
) -> Result<(TransactionSnapshot, bool), TransactionError> {
    registration::accepted(snapshot).map_err(|_| TransactionError::InvalidState)?;
    check_message_id(snapshot, message_id, &observation.fingerprint, true)?;
    let mut transaction = snapshot
        .transactions
        .iter()
        .find(|t| {
            t.ocpp16
                .as_ref()
                .is_some_and(|s| s.transaction_id == observation.transaction_id)
        })
        .ok_or(TransactionError::InvalidState)?
        .clone();
    let state = transaction
        .ocpp16
        .as_mut()
        .ok_or(TransactionError::InvalidState)?;
    if let Some(fingerprint) = &state.stop_fingerprint {
        return if fingerprint == &observation.fingerprint {
            Ok((transaction, true))
        } else {
            Err(TransactionError::Conflict)
        };
    }
    if observation.occurred_at < transaction.started_at
        || snapshot.transactions.iter().any(|t| {
            t.ocpp16.as_ref().is_some_and(|s| {
                s.start_message_id == message_id || s.stop_message_id.as_deref() == Some(message_id)
            })
        })
    {
        return Err(TransactionError::Conflict);
    }
    state.stop_message_id = Some(message_id.to_owned());
    state.stop_fingerprint = Some(observation.fingerprint.clone());
    state
        .stop_identity_fingerprint
        .clone_from(&observation.identity_fingerprint);
    state.meter_stop = Some(observation.meter_stop);
    state.stop_reason.clone_from(&observation.reason);
    state.transaction_data.clone_from(&observation.values);
    // StopTransaction has no connectorId; bind all meter references to the known transaction.
    for value in &mut state.transaction_data {
        if let Some(NativeProtocolReference::Ocpp16 { connector_id }) =
            transaction.resource.native_protocol_reference
        {
            value.point_id = PointId::new(value.point_id.as_str().replacen(
                "ocpp16/connector-0/",
                &format!("ocpp16/connector-{connector_id}/"),
                1,
            ))
            .map_err(|_| TransactionError::InvalidState)?;
        }
        if let Some(measurement) = &mut value.measurement {
            measurement.protocol_reference = transaction.resource.native_protocol_reference;
        }
    }
    state.signed_values.clone_from(&observation.signed_values);
    transaction.state = TransactionState::Ended;
    transaction.ended_at = Some(observation.occurred_at);
    Ok((transaction, false))
}

/// Publishes state, critical event and selected-target delivery as one storage transaction.
/// The caller holds the single station owner across preparation, commit and snapshot replacement.
/// # Errors
/// Leaves the caller's snapshot untouched on any failed commit or invalid trusted context.
pub async fn commit<C: Send + 'static, R: Send + 'static>(
    store: &dyn OperationalStore<C, TransactionSnapshot, TransactionSnapshot, R>,
    snapshot: &mut StationSnapshot,
    transaction: TransactionSnapshot,
    context: TransactionContext,
    now: UtcTimestamp,
) -> Result<(), TransactionError> {
    if context.identity.bridge_id != snapshot.station.bridge_id
        || context.identity.selected_target_id != context.target.as_ref().map(|t| t.0.clone())
    {
        return Err(TransactionError::InvalidState);
    }
    let ended = transaction.state == TransactionState::Ended;
    let mut next = snapshot.clone();
    retention::prune(&mut next, now);
    if let Some(existing) = next
        .transactions
        .iter_mut()
        .find(|t| t.transaction_id == transaction.transaction_id)
    {
        *existing = transaction.clone();
    } else {
        next.transactions.push(transaction.clone());
    }
    registration::activity(&mut next, now);
    let event = EventEnvelope {
        event_id: context.event_id.clone(),
        schema_version: snapshot.schema_version,
        runtime: context.identity.runtime,
        resource: transaction.resource.clone(),
        source_time: transaction.ended_at.or(Some(transaction.started_at)),
        observed_at: now,
        event_type: EventType::new(if ended {
            "transaction.ended"
        } else {
            "transaction.started"
        })
        .map_err(|_| TransactionError::InvalidState)?,
        origin: EventOrigin::Station,
        sequence: context.sequence,
        correlation_id: context.correlation_id,
        causation_id: None,
        provenance: None,
        payload: transaction.clone(),
    };
    let mut write = AtomicStoreWrite::empty();
    write.purpose = if ended {
        StorageWritePurpose::ActiveSessionCompletion
    } else {
        StorageWritePurpose::NewSessionStart
    };
    write.station_snapshot = Some(next.clone());
    write.journal_events.push(event);
    if let Some((target, revision)) = context.target {
        write.required_deliveries.push(PendingDelivery {
            delivery_id: DeliveryId::new(format!("transaction/{}", context.event_id.as_str()))
                .map_err(TransactionError::Storage)?,
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
        .map_err(TransactionError::Storage)?;
    *snapshot = next;
    Ok(())
}

// Validate identity conflicts before fingerprint replay, including IDs from another action.
fn check_message_id(
    snapshot: &StationSnapshot,
    message_id: &str,
    fingerprint: &str,
    stopping: bool,
) -> Result<(), TransactionError> {
    for state in snapshot
        .transactions
        .iter()
        .filter_map(|t| t.ocpp16.as_ref())
    {
        if (state.start_message_id == message_id
            && (stopping || state.start_fingerprint != fingerprint))
            || (state.stop_message_id.as_deref() == Some(message_id)
                && (!stopping || state.stop_fingerprint.as_deref() != Some(fingerprint)))
        {
            return Err(TransactionError::Conflict);
        }
    }
    Ok(())
}
