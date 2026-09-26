use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{CommandAdmissionOutcome, StorageError, StorageErrorCode};
use uob_contracts::{Command, CommandLifecycle, EventEnvelope};

use crate::{
    codec::{self, EncodedCommand, EncodedCommandResult},
    configuration::unavailable,
};

pub(crate) fn admit(
    transaction: &Transaction<'_>,
    command: Option<&EncodedCommand>,
) -> Result<Option<CommandAdmissionOutcome>, StorageError> {
    let Some(command) = command else {
        return Ok(None);
    };
    let existing = transaction
        .query_row(
            "SELECT fingerprint, payload FROM commands WHERE request_id = ?1",
            [&command.request_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    if let Some((stored_fingerprint, stored_payload)) = existing {
        let fingerprint =
            stored_fingerprint.map_or_else(|| codec::command_fingerprint(&stored_payload), Ok)?;
        return if fingerprint == command.fingerprint {
            let result = transaction
                .query_row(
                    "SELECT payload FROM command_results WHERE request_id = ?1",
                    [&command.request_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(unavailable)?
                .map(|payload| codec::decode_result(&payload).map(Box::new))
                .transpose()?;
            Ok(Some(CommandAdmissionOutcome::Duplicate { result }))
        } else {
            Err(StorageError::new(
                StorageErrorCode::Conflict,
                "request ID is already associated with another command",
            ))
        };
    }
    transaction
        .execute(
            "INSERT INTO commands(\n\
                 request_id, fingerprint, admitted_at, retain_until, unresolved, payload\n\
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![
                command.request_id,
                command.fingerprint,
                command.admitted_at,
                command.retain_until,
                command.payload
            ],
        )
        .map_err(unavailable)?;
    Ok(Some(CommandAdmissionOutcome::Admitted))
}

pub(crate) fn write_result(
    transaction: &Transaction<'_>,
    encoded: &EncodedCommandResult,
) -> Result<(), StorageError> {
    let mut incoming = codec::decode_result(&encoded.payload)?;
    let previous = transaction
        .query_row(
            "SELECT payload FROM command_results WHERE request_id = ?1",
            [&encoded.request_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?
        .map(|payload| codec::decode_result(&payload))
        .transpose()?;

    if let Some(mut previous) = previous {
        if previous.return_route != incoming.return_route || previous.resource != incoming.resource
        {
            return Err(StorageError::new(
                StorageErrorCode::IntegrityFailure,
                "command result identity changed",
            ));
        }
        for effect in previous.observed_effects.drain(..) {
            if !incoming
                .observed_effects
                .iter()
                .any(|item| item.event_id == effect.event_id)
            {
                incoming.observed_effects.push(effect);
            }
        }
        // An effect writer may have read Dispatched before a response writer committed.
        // Only the station response advances the lifecycle; an old effect cannot roll it back.
        let previous_rank = lifecycle_rank(&previous.lifecycle);
        let incoming_rank = lifecycle_rank(&incoming.lifecycle);
        if previous_rank > incoming_rank || (previous_rank == 2 && incoming_rank == 2) {
            incoming.lifecycle = previous.lifecycle;
            incoming.recorded_at = previous.recorded_at;
            incoming.schema_version = previous.schema_version;
            incoming.configuration = previous.configuration;
        }
        for observation in previous.configuration_observations {
            if !incoming
                .configuration_observations
                .iter()
                .any(|item| item.read_request_id == observation.read_request_id)
            {
                incoming.configuration_observations.push(observation);
            }
        }
    }

    let unresolved = matches!(
        incoming.lifecycle,
        CommandLifecycle::Admitted
            | CommandLifecycle::Dispatched
            | CommandLifecycle::TransmissionUncertain { .. }
    );
    let payload = serde_json::to_string(&incoming).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "command result encoding failed",
        )
    })?;
    transaction
        .execute(
            "INSERT INTO command_results(request_id, payload) VALUES (?1, ?2)
         ON CONFLICT(request_id) DO UPDATE SET payload = excluded.payload",
            params![encoded.request_id, payload],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE commands SET unresolved = ?2 WHERE request_id = ?1",
            params![encoded.request_id, i64::from(unresolved)],
        )
        .map_err(unavailable)?;
    Ok(())
}

fn lifecycle_rank(value: &CommandLifecycle) -> u8 {
    match value {
        CommandLifecycle::Admitted => 0,
        CommandLifecycle::Dispatched => 1,
        CommandLifecycle::Rejected { .. }
        | CommandLifecycle::ProtocolResponse { .. }
        | CommandLifecycle::TransmissionUncertain { .. } => 2,
    }
}

pub(crate) fn candidates<C: DeserializeOwned>(
    connection: &Connection,
    bridge: &str,
    station: &str,
    observed_before: i64,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<Command<C>>, StorageError> {
    let limit = i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "command candidate limit exceeds SQLite integer range",
        )
    })?;

    let mut statement = connection
        .prepare(
            "SELECT payload FROM commands INDEXED BY commands_station_history
         WHERE json_extract(payload, '$.resource.bridge_id') = ?1
           AND json_extract(payload, '$.resource.station_id') = ?2
           AND admitted_at <= ?3
           AND (?4 IS NULL OR (admitted_at, request_id) <
                (SELECT admitted_at, request_id FROM commands WHERE request_id = ?4))
         ORDER BY admitted_at DESC, request_id DESC LIMIT ?5",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![bridge, station, observed_before, after, limit],
            |row| row.get::<_, String>(0),
        )
        .map_err(unavailable)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?
        .into_iter()
        .map(|payload| codec::decode_command(&payload))
        .collect()
}

pub(crate) fn journal_event<E: DeserializeOwned>(
    connection: &Connection,
    id: &str,
    bridge: &str,
    station: &str,
) -> Result<Option<EventEnvelope<E>>, StorageError> {
    connection
        .query_row(
            "SELECT payload FROM journal_events WHERE event_id = ?1
         AND json_extract(payload, '$.resource.bridge_id') = ?2
         AND json_extract(payload, '$.resource.station_id') = ?3",
            params![id, bridge, station],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?
        .map(|payload| codec::decode_event(&payload))
        .transpose()
}

pub(crate) fn prune<C: DeserializeOwned + Serialize>(
    connection: &mut Connection,
    now: i64,
) -> Result<u64, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    backfill_metadata::<C>(&transaction)?;
    let mut statement = transaction
        .prepare(
            "SELECT request_id FROM commands\n\
             WHERE unresolved = 0 AND retain_until IS NOT NULL AND retain_until <= ?1",
        )
        .map_err(unavailable)?;
    let request_ids = statement
        .query_map([now], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    drop(statement);
    for request_id in &request_ids {
        transaction
            .execute(
                "DELETE FROM command_results WHERE request_id = ?1",
                [request_id],
            )
            .map_err(unavailable)?;
        transaction
            .execute("DELETE FROM commands WHERE request_id = ?1", [request_id])
            .map_err(unavailable)?;
    }
    transaction.commit().map_err(unavailable)?;
    u64::try_from(request_ids.len()).map_err(|_| {
        StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "pruned command count exceeds supported range",
        )
    })
}

fn backfill_metadata<C: DeserializeOwned + Serialize>(
    transaction: &Transaction<'_>,
) -> Result<(), StorageError> {
    let mut statement = transaction
        .prepare(
            "SELECT request_id, payload FROM commands\n\
             WHERE fingerprint IS NULL OR admitted_at IS NULL OR retain_until IS NULL",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    drop(statement);
    for (request_id, payload) in rows {
        let command = codec::decode_command::<C>(&payload)?;
        let encoded = codec::encode_command(&command)?;
        if encoded.request_id != request_id {
            return Err(StorageError::new(
                StorageErrorCode::IntegrityFailure,
                "stored command request ID does not match its key",
            ));
        }
        let unresolved = transaction
            .query_row(
                "SELECT payload FROM command_results WHERE request_id = ?1",
                [&request_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(unavailable)?
            .map(|payload| codec::decode_result(&payload))
            .transpose()?
            .is_none_or(|result| {
                matches!(
                    result.lifecycle,
                    CommandLifecycle::Admitted
                        | CommandLifecycle::Dispatched
                        | CommandLifecycle::TransmissionUncertain { .. }
                )
            });
        transaction
            .execute(
                "UPDATE commands SET fingerprint = ?2, admitted_at = ?3, retain_until = ?4,\n\
                     unresolved = ?5 WHERE request_id = ?1",
                params![
                    request_id,
                    encoded.fingerprint,
                    encoded.admitted_at,
                    encoded.retain_until,
                    i64::from(unresolved)
                ],
            )
            .map_err(unavailable)?;
    }
    Ok(())
}
