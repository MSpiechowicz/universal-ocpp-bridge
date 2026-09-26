use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, EventEnvelope, EventOrigin, RequestId,
    ResourceRef, StationEvent, TriggerMessageClass, TriggerObservationStatus, TriggerObservedEvent,
    UtcTimestamp,
};

use crate::{codec, configuration::unavailable, snapshots};

const MAX_TARGETS: usize = 65;

struct MatchingEventScope<'a> {
    station: &'a str,
    resource: &'a ResourceRef,
    requested_class: TriggerMessageClass,
    start: &'a str,
    deadline: &'a str,
    started_at: UtcTimestamp,
    ends_at: UtcTimestamp,
}

fn invalid(detail: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::IntegrityFailure, detail)
}

pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(stored) = &previous.trigger_observation else {
        return Ok(());
    };
    if matches!(incoming.lifecycle, CommandLifecycle::Rejected { .. })
        && incoming.trigger_observation.is_none()
    {
        return Ok(());
    }
    let Some(next) = incoming.trigger_observation.as_mut() else {
        incoming.trigger_observation = Some(stored.clone());
        incoming.schema_version = ContractVersion::V1_TRIGGER;
        return Ok(());
    };
    if stored.requested_class != next.requested_class
        || stored.native_scope != next.native_scope
        || stored.expected_targets != next.expected_targets
        || stored.dispatch_started_at != next.dispatch_started_at
        || stored.deadline != next.deadline
    {
        return Err(invalid("trigger dispatch expectation changed"));
    }
    if stored.native_response.is_some()
        && next.native_response.is_some()
        && stored.native_response != next.native_response
    {
        return Err(invalid("trigger native response changed"));
    }
    next.native_response = next.native_response.or(stored.native_response);
    for evidence in &stored.observed {
        if !next
            .observed
            .iter()
            .any(|event| event.event_id == evidence.event_id)
        {
            next.observed.push(evidence.clone());
        }
    }
    if next.observed.len() > MAX_TARGETS {
        return Err(invalid("trigger evidence bound exceeded"));
    }
    let at = incoming
        .recorded_at
        .max(if stored.status == TriggerObservationStatus::Absent {
            stored.deadline
        } else {
            stored.dispatch_started_at
        });
    next.refresh_status(at);
    incoming.schema_version = ContractVersion::V1_TRIGGER;
    Ok(())
}

/// Exposes only live trigger windows; ordinary station updates need no extra marker.
pub(crate) fn pending(
    connection: &Connection,
    station: &ResourceRef,
    class: TriggerMessageClass,
    now: UtcTimestamp,
) -> Result<bool, StorageError> {
    let now = serde_json::to_string(&now).map_err(|_| invalid("trigger timestamp invalid"))?;
    let found = connection.query_row(
        "SELECT 1 FROM command_results r JOIN commands c ON c.request_id = r.request_id
         WHERE r.trigger_reconcile_active = 1
           AND json_extract(c.payload,'$.resource.bridge_id') = ?1
           AND json_extract(c.payload,'$.resource.station_id') = ?2
           AND json_extract(r.payload,'$.trigger_observation.requested_class') = ?3
           AND julianday(json_extract(r.payload,'$.trigger_observation.deadline')) >= julianday(json_extract(?4,'$'))
           AND json_extract(r.payload,'$.trigger_observation.status') IN ('pending','partial')
         LIMIT 1",
        params![station.bridge_id.as_str(), station.station_id.as_str(), class.as_str(), now],
        |_| Ok(()),
    ).optional().map_err(unavailable)?;
    Ok(found.is_some())
}

/// Stable, bounded pending-result page; callers repeat with last returned request ID.
pub(crate) fn candidates(
    connection: &Connection,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<RequestId>, StorageError> {
    let limit = i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "trigger candidate limit exceeds SQLite integer range",
        )
    })?;

    let mut statement = connection
        .prepare(
            "SELECT request_id FROM command_results
         WHERE request_id > ?1
           AND trigger_reconcile_active = 1
           AND json_extract(payload,'$.trigger_observation.status') IN ('pending','partial')
         ORDER BY request_id LIMIT ?2",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![after.unwrap_or(""), limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(unavailable)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?
        .into_iter()
        .map(|value| RequestId::new(value).map_err(|_| invalid("trigger request ID invalid")))
        .collect()
}

/// One immediate SQLite transaction closes response/effect/deadline races.
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
    if result.trigger_observation.is_some() {
        validate_trigger_command(&transaction, request_id, &result.resource)?;
    }
    if active == 0 {
        transaction.commit().map_err(unavailable)?;
        return Ok(Some(result));
    }
    if let Some(observation) = result.trigger_observation.as_mut() {
        if observation.expected_targets.is_empty()
            || observation.expected_targets.len() > MAX_TARGETS
        {
            return Err(invalid("trigger targets invalid"));
        }
        let station = snapshots::station_key(&ResourceRef {
            bridge_id: result.resource.bridge_id.clone(),
            station_id: result.resource.station_id.clone(),
            resource: None,
            native_protocol_reference: None,
        })?;
        let start = serde_json::to_string(&observation.dispatch_started_at)
            .map_err(|_| invalid("trigger timestamp invalid"))?;
        let deadline = serde_json::to_string(&observation.deadline)
            .map_err(|_| invalid("trigger timestamp invalid"))?;
        let scope = MatchingEventScope {
            station: &station,
            resource: &result.resource,
            requested_class: observation.requested_class,
            start: &start,
            deadline: &deadline,
            started_at: observation.dispatch_started_at,
            ends_at: observation.deadline,
        };
        let previous_status = observation.status;
        let mut changed = false;
        for index in 0..observation.expected_targets.len() {
            let target = observation.expected_targets[index];
            if observation
                .observed
                .iter()
                .any(|event| event.target == target)
            {
                continue;
            }
            if let Some(event) = matching_event(&transaction, &scope, target)? {
                observation.observed.push(event);
                changed = true;
            }
        }
        observation.refresh_status(now);
        let finalized = now >= observation.deadline;
        if changed || previous_status != observation.status || finalized {
            let encoded =
                serde_json::to_string(&result).map_err(|_| invalid("trigger result invalid"))?;
            transaction
                .execute(
                    "UPDATE command_results SET payload = ?2, trigger_reconcile_active = ?3 WHERE request_id = ?1",
                    params![request_id, encoded, i64::from(!finalized)],
                )
                .map_err(unavailable)?;
        }
    }
    transaction.commit().map_err(unavailable)?;
    Ok(Some(result))
}

fn validate_trigger_command(
    transaction: &rusqlite::Transaction<'_>,
    request_id: &str,
    resource: &ResourceRef,
) -> Result<(), StorageError> {
    let original = transaction
        .query_row(
            "SELECT 1 FROM commands
         WHERE request_id = ?1
           AND json_extract(payload,'$.operation.kind') = 'ocpp'
           AND json_extract(payload,'$.operation.parameters.protocol') = 'ocpp16j'
           AND json_extract(payload,'$.operation.parameters.action') = 'TriggerMessage'
           AND json_extract(payload,'$.resource.bridge_id') = ?2
           AND json_extract(payload,'$.resource.station_id') = ?3",
            params![
                request_id,
                resource.bridge_id.as_str(),
                resource.station_id.as_str(),
            ],
            |_| Ok(()),
        )
        .optional()
        .map_err(unavailable)?;
    if original.is_none() {
        return Err(invalid("trigger result has no matching OCPP 1.6 command"));
    }
    Ok(())
}

fn matching_event(
    transaction: &rusqlite::Transaction<'_>,
    scope: &MatchingEventScope<'_>,
    target: u32,
) -> Result<Option<TriggerObservedEvent>, StorageError> {
    let connector = if matches!(
        scope.requested_class,
        TriggerMessageClass::StatusNotification | TriggerMessageClass::MeterValues
    ) {
        Some(i64::from(target))
    } else {
        None
    };
    let mut statement = transaction
        .prepare(
            "SELECT payload FROM journal_events INDEXED BY trigger_station_events
         WHERE resource = ?1
           AND json_extract(payload,'$.payload.class') = ?2
           AND json_extract(payload,'$.payload.connector_id') IS ?3
           AND julianday(json_extract(payload,'$.observed_at')) >= julianday(json_extract(?4,'$'))
           AND julianday(json_extract(payload,'$.observed_at')) <= julianday(json_extract(?5,'$'))
           AND json_extract(payload,'$.origin.kind') = 'station'
           AND json_valid(payload)
           AND json_type(payload,'$.payload.class') IS NOT NULL
           AND json_type(payload,'$.provenance') IS NULL
         ORDER BY julianday(json_extract(payload,'$.observed_at')), row_id LIMIT 128",
        )
        .map_err(unavailable)?;
    let mut rows = statement
        .query(params![
            scope.station,
            scope.requested_class.as_str(),
            connector,
            scope.start,
            scope.deadline
        ])
        .map_err(unavailable)?;
    while let Some(row) = rows.next().map_err(unavailable)? {
        let payload: String = row.get(0).map_err(unavailable)?;
        let event: EventEnvelope<StationEvent> =
            serde_json::from_str(&payload).map_err(|_| invalid("trigger event invalid"))?;
        let StationEvent::TriggerNotification {
            class,
            connector_id,
            station_snapshot_invalidated,
            ..
        } = event.payload
        else {
            return Err(invalid("trigger marker payload invalid"));
        };
        if class != scope.requested_class
            || event.origin != EventOrigin::Station
            || event.provenance.is_some()
            || station_snapshot_invalidated != scope.resource.station_id
            || event.resource.bridge_id != scope.resource.bridge_id
            || event.resource.station_id != scope.resource.station_id
            || connector_id != connector.map(|_| target)
        {
            return Err(invalid("trigger evidence mismatch"));
        }
        // SQLite's julianday has millisecond precision. The typed comparison prevents
        // sub-millisecond evidence before dispatch or after deadline becoming false success.
        if event.observed_at < scope.started_at || event.observed_at > scope.ends_at {
            continue;
        }
        return Ok(Some(TriggerObservedEvent {
            event_id: event.event_id,
            target,
            observed_at: event.observed_at,
        }));
    }
    Ok(None)
}
