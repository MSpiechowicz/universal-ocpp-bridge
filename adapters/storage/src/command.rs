use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{CommandAdmissionOutcome, StorageError, StorageErrorCode};
use uob_contracts::CommandLifecycle;

use crate::{
    codec::{self, EncodedCommand},
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
