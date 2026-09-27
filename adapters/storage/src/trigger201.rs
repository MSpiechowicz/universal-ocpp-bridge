use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, EventEnvelope, EventOrigin, ResourceRef,
    StationEvent, TriggerMessageClass201, TriggerObservation201, TriggerObservationStatus201,
    TriggerObservedEvent201, TriggerTarget201, UtcTimestamp,
};

use crate::{codec, configuration::unavailable, snapshots};

const MAX_TARGETS: usize = 65;

fn invalid(detail: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::IntegrityFailure, detail)
}

/// Returns whether a definitely unsent dispatch must leave the reconciliation index.
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<bool, StorageError> {
    let Some(stored) = &previous.trigger_observation_201 else {
        // A stale dispatch writer cannot resurrect an expectation after terminal rejection.
        if matches!(previous.lifecycle, CommandLifecycle::Rejected { .. }) {
            incoming.trigger_observation_201 = None;
        }
        return Ok(false);
    };
    if matches!(previous.lifecycle, CommandLifecycle::Dispatched)
        && stored.native_response.is_none()
        && matches!(incoming.lifecycle, CommandLifecycle::Rejected { .. })
        && incoming.trigger_observation_201.is_none()
    {
        return Ok(true);
    }
    let Some(next) = incoming.trigger_observation_201.as_mut() else {
        incoming.trigger_observation_201 = Some(stored.clone());
        incoming.schema_version = ContractVersion::V1_TRIGGER_201;
        return Ok(false);
    };
    if stored.requested_class != next.requested_class
        || stored.native_scope != next.native_scope
        || stored.expected_targets != next.expected_targets
        || stored.dispatch_started_at != next.dispatch_started_at
        || stored.deadline != next.deadline
    {
        return Err(invalid("OCPP 2.0.1 trigger expectation changed"));
    }
    if stored.native_response.is_some()
        && next.native_response.is_some()
        && stored.native_response != next.native_response
    {
        return Err(invalid("OCPP 2.0.1 trigger response changed"));
    }
    if next
        .observed
        .iter()
        .any(|event| !stored.observed.contains(event))
    {
        return Err(invalid(
            "OCPP 2.0.1 trigger evidence changed outside reconciliation",
        ));
    }
    next.native_response = next
        .native_response
        .clone()
        .or_else(|| stored.native_response.clone());
    for event in &stored.observed {
        if !next
            .observed
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            next.observed.push(event.clone());
        }
    }
    if next.observed.len() > MAX_TARGETS + usize::from(stored.requested_class.is_certificate()) {
        return Err(invalid("OCPP 2.0.1 trigger evidence bound exceeded"));
    }
    let at = incoming
        .recorded_at
        .max(if stored.status == TriggerObservationStatus201::Absent {
            stored.deadline
        } else {
            stored.dispatch_started_at
        });
    next.refresh_status(at);
    incoming.schema_version = ContractVersion::V1_TRIGGER_201;
    Ok(false)
}

pub(crate) fn pending(
    connection: &Connection,
    station: &ResourceRef,
    class: TriggerMessageClass201,
    now: UtcTimestamp,
) -> Result<bool, StorageError> {
    let now = serde_json::to_string(&now).map_err(|_| invalid("trigger timestamp invalid"))?;
    let found = connection.query_row(
        "SELECT 1 FROM command_results r JOIN commands c ON c.request_id = r.request_id
         WHERE r.trigger_reconcile_active = 1
           AND json_extract(c.payload,'$.resource.bridge_id') = ?1
           AND json_extract(c.payload,'$.resource.station_id') = ?2
           AND json_extract(c.payload,'$.operation.kind') = 'ocpp'
           AND json_extract(c.payload,'$.operation.parameters.protocol') = 'ocpp201'
           AND json_extract(c.payload,'$.operation.parameters.action') = 'TriggerMessage'
           AND json_extract(r.payload,'$.trigger_observation_201.requested_class') = ?3
           AND julianday(json_extract(r.payload,'$.trigger_observation_201.dispatch_started_at')) <= julianday(json_extract(?4,'$'))
           AND julianday(json_extract(r.payload,'$.trigger_observation_201.deadline')) >= julianday(json_extract(?4,'$'))
           AND json_extract(r.payload,'$.trigger_observation_201.status') IN ('pending','partial','unattributable')
         LIMIT 1",
        params![station.bridge_id.as_str(), station.station_id.as_str(), class.as_str(), now],
        |_| Ok(()),
    ).optional().map_err(unavailable)?;
    Ok(found.is_some())
}

pub(crate) fn reconcile(
    connection: &mut Connection,
    request_id: &str,
    now: UtcTimestamp,
) -> Result<Option<CommandResult>, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let stored = transaction
        .query_row(
            "SELECT payload, trigger_reconcile_active FROM command_results WHERE request_id = ?1",
            [request_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, active)) = stored else {
        return Ok(None);
    };
    let mut result = codec::decode_result(&payload)?;
    if result.return_route.request_id.as_str() != request_id {
        return Err(invalid("trigger result identity mismatch"));
    }
    if result.trigger_observation_201.is_some() {
        validate_command(&transaction, request_id, &result.resource)?;
    }
    if active == 0 {
        transaction.commit().map_err(unavailable)?;
        return Ok(Some(result));
    }
    if let Some(observation) = result.trigger_observation_201.as_mut() {
        if observation.expected_targets.is_empty()
            || observation.expected_targets.len() > MAX_TARGETS
            || observation.observed.len() > MAX_TARGETS + 1
            || observation.dispatch_started_at > observation.deadline
        {
            return Err(invalid("OCPP 2.0.1 trigger targets or window invalid"));
        }
        let station = snapshots::station_key(&ResourceRef {
            bridge_id: result.resource.bridge_id.clone(),
            station_id: result.resource.station_id.clone(),
            resource: None,
            native_protocol_reference: None,
        })?;
        let previous_status = observation.status;
        let mut changed = false;
        for target in observation.expected_targets.clone() {
            if observation
                .observed
                .iter()
                .any(|event| event.target == target)
            {
                continue;
            }
            if let Some(event) = matching_event(
                &transaction,
                &station,
                &result.resource,
                observation,
                target,
            )? && !observation
                .observed
                .iter()
                .any(|seen| seen.event_id == event.event_id)
            {
                observation.observed.push(event);
                changed = true;
            }
        }
        // SignCertificate carries no EVSE identity. Preserve the station receipt without
        // crediting it to the scoped EVSE expected by the command.
        if observation.requested_class.is_certificate()
            && !observation.requested_class.is_station_only()
            && observation.native_scope.is_some()
            && !observation
                .observed
                .iter()
                .any(|event| event.target == TriggerTarget201::Station)
            && let Some(event) = matching_event(
                &transaction,
                &station,
                &result.resource,
                observation,
                TriggerTarget201::Station,
            )?
        {
            observation.observed.push(event);
            changed = true;
        }
        observation.refresh_status(now);
        let finalized = now >= observation.deadline
            || observation.status == TriggerObservationStatus201::Observed;
        if changed || previous_status != observation.status || finalized {
            let encoded =
                serde_json::to_string(&result).map_err(|_| invalid("trigger result invalid"))?;
            transaction.execute(
                "UPDATE command_results SET payload = ?2, trigger_reconcile_active = ?3 WHERE request_id = ?1",
                params![request_id, encoded, i64::from(!finalized)],
            ).map_err(unavailable)?;
        }
    }
    transaction.commit().map_err(unavailable)?;
    Ok(Some(result))
}

fn validate_command(
    transaction: &Transaction<'_>,
    request_id: &str,
    resource: &ResourceRef,
) -> Result<(), StorageError> {
    let original = transaction
        .query_row(
            "SELECT 1 FROM commands WHERE request_id = ?1
           AND json_extract(payload,'$.operation.kind') = 'ocpp'
           AND json_extract(payload,'$.operation.parameters.protocol') = 'ocpp201'
           AND json_extract(payload,'$.operation.parameters.action') = 'TriggerMessage'
           AND json_extract(payload,'$.resource.bridge_id') = ?2
           AND json_extract(payload,'$.resource.station_id') = ?3",
            params![
                request_id,
                resource.bridge_id.as_str(),
                resource.station_id.as_str()
            ],
            |_| Ok(()),
        )
        .optional()
        .map_err(unavailable)?;
    if original.is_none() {
        return Err(invalid("trigger result has no matching OCPP 2.0.1 command"));
    }
    Ok(())
}

fn matching_event(
    transaction: &Transaction<'_>,
    station: &str,
    resource: &ResourceRef,
    observation: &TriggerObservation201,
    target: TriggerTarget201,
) -> Result<Option<TriggerObservedEvent201>, StorageError> {
    let (kind, id, connector) = match target {
        TriggerTarget201::Station => ("station", None, None),
        TriggerTarget201::Evse { id } => ("evse", Some(i64::from(id)), None),
        TriggerTarget201::Connector { id, connector_id } => (
            "connector",
            Some(i64::from(id)),
            Some(i64::from(connector_id)),
        ),
    };
    let start = serde_json::to_string(&observation.dispatch_started_at)
        .map_err(|_| invalid("trigger timestamp invalid"))?;
    let deadline = serde_json::to_string(&observation.deadline)
        .map_err(|_| invalid("trigger timestamp invalid"))?;
    let mut statement = transaction
        .prepare(
            "SELECT payload FROM journal_events INDEXED BY trigger201_station_events
         WHERE resource = ?1
           AND json_extract(payload,'$.payload.trigger_class_201') = ?2
           AND (json_extract(payload,'$.payload.target.kind') = ?3
                OR (?3 = 'evse' AND ?2 = 'TransactionEvent'
                    AND json_extract(payload,'$.payload.target.kind') = 'connector'))
           AND json_extract(payload,'$.payload.target.id') IS ?4
           AND (?3 = 'evse' AND ?2 = 'TransactionEvent'
                OR json_extract(payload,'$.payload.target.connector_id') IS ?5)
           AND julianday(json_extract(payload,'$.observed_at')) >= julianday(json_extract(?6,'$'))
           AND julianday(json_extract(payload,'$.observed_at')) <= julianday(json_extract(?7,'$'))
           AND json_extract(payload,'$.origin.kind') = 'station'
           AND json_valid(payload)
           AND json_type(payload,'$.payload.trigger_class_201') IS NOT NULL
           AND (?2 != 'TransactionEvent' OR json_extract(payload,'$.payload.status') = 'Trigger')
           AND json_type(payload,'$.provenance') IS NULL
         ORDER BY julianday(json_extract(payload,'$.observed_at')), row_id LIMIT 128",
        )
        .map_err(unavailable)?;
    let mut rows = statement
        .query(params![
            station,
            observation.requested_class.as_str(),
            kind,
            id,
            connector,
            start,
            deadline
        ])
        .map_err(unavailable)?;
    while let Some(row) = rows.next().map_err(unavailable)? {
        let payload: String = row.get(0).map_err(unavailable)?;
        let event: EventEnvelope<StationEvent> =
            serde_json::from_str(&payload).map_err(|_| invalid("trigger event invalid"))?;
        let StationEvent::TriggerNotification201 {
            station_snapshot_invalidated,
            trigger_class_201,
            target: event_target,
            status,
        } = event.payload
        else {
            return Err(invalid("OCPP 2.0.1 trigger marker invalid"));
        };
        let target_matches = event_target == target
            || (observation.requested_class == TriggerMessageClass201::TransactionEvent
                && matches!(
                    (target, event_target),
                    (TriggerTarget201::Evse { id }, TriggerTarget201::Connector { id: actual, .. })
                        if id == actual
                ));
        if event.origin != EventOrigin::Station
            || event.provenance.is_some()
            || station_snapshot_invalidated != resource.station_id
            || event.resource.bridge_id != resource.bridge_id
            || event.resource.station_id != resource.station_id
            || trigger_class_201 != observation.requested_class
            || !target_matches
        {
            return Err(invalid("OCPP 2.0.1 trigger evidence mismatch"));
        }
        if event.observed_at < observation.dispatch_started_at
            || event.observed_at > observation.deadline
        {
            continue;
        }
        if observation.requested_class == TriggerMessageClass201::TransactionEvent
            && status.as_deref() != Some("Trigger")
        {
            continue;
        }
        if observation
            .observed
            .iter()
            .any(|seen| seen.event_id == event.event_id)
        {
            continue;
        }
        return Ok(Some(TriggerObservedEvent201 {
            event_id: event.event_id,
            target,
            observed_at: event.observed_at,
        }));
    }
    Ok(None)
}
