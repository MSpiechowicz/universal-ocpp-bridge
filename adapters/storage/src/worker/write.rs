use crate::{
    codec::{EncodedAuthorization, EncodedDelivery, EncodedEvent, EncodedRecord, EncodedWrite},
    command,
    configuration::unavailable,
    retention::{self, SqliteRetentionPolicy},
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use uob_application::{
    AtomicWriteOutcome, CommandAdmissionOutcome, StorageError, StorageErrorCode,
};
pub(super) fn write_atomic(
    connection: &mut Connection,
    retention_policy: SqliteRetentionPolicy,
    mut write: EncodedWrite,
) -> Result<AtomicWriteOutcome, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let command = command::admit(&transaction, write.command.as_ref())?;
    if matches!(
        command.as_ref(),
        Some(CommandAdmissionOutcome::Duplicate { .. })
    ) {
        return Ok(AtomicWriteOutcome { command });
    }
    retention::prepare_write(&transaction, retention_policy, &mut write)?;
    if let Some(reservation) = write.charging_profile_201.as_deref() {
        crate::charging_profile201::reserve(&transaction, reservation)?;
    }
    if let Some(mutation) = write.reservation_16.as_deref() {
        crate::reservation16::reserve(&transaction, mutation)?;
    }
    for observation in &write.reservation_observations_16 {
        crate::reservation16::observe(&transaction, observation)?;
    }
    if let Some((station, payload)) = write.snapshot {
        crate::charging_profile201::retire_ended(
            &transaction,
            &crate::codec::decode_snapshot(&payload)?,
        )?;
        transaction
            .execute(
                "INSERT INTO station_snapshots(station_key, payload) VALUES (?1, ?2)\n\
                 ON CONFLICT(station_key) DO UPDATE SET payload = excluded.payload",
                params![station, payload],
            )
            .map_err(unavailable)?;
    }
    for change in write.authorization {
        write_authorization(&transaction, &change)?;
    }
    if let Some(result) = write.command_result {
        command::write_result(&transaction, &result)?;
    }
    if let Some(sequence) = write.events.iter().map(|event| event.sequence).max() {
        transaction
            .execute(
                "UPDATE event_sequence_counter SET value = MAX(value, ?1) WHERE id = 1",
                [sequence],
            )
            .map_err(unavailable)?;
    }
    for event in write.events {
        write_event(&transaction, &event)?;
    }
    for delivery in write.deliveries {
        write_delivery(&transaction, &delivery)?;
    }
    for record in write.records {
        write_record(&transaction, &record)?;
    }
    transaction.commit().map_err(unavailable)?;
    Ok(AtomicWriteOutcome { command })
}

fn write_authorization(
    transaction: &Transaction<'_>,
    value: &EncodedAuthorization,
) -> Result<(), StorageError> {
    let changed = transaction
        .execute(
            "INSERT INTO authorization_changes(reference, resource, state, revision, changed_at, expires_at)\n\
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)\n\
             ON CONFLICT(reference) DO UPDATE SET resource = excluded.resource,\n\
                 state = excluded.state, revision = excluded.revision,\n\
                 changed_at = excluded.changed_at, expires_at = excluded.expires_at\n\
             WHERE excluded.revision > authorization_changes.revision\n\
                OR (excluded.revision = authorization_changes.revision\n\
                    AND excluded.resource = authorization_changes.resource\n\
                    AND excluded.state = authorization_changes.state\n\
                    AND excluded.changed_at = authorization_changes.changed_at\n\
                    AND excluded.expires_at IS authorization_changes.expires_at)",
            params![
                value.reference,
                value.resource,
                value.state,
                value.revision,
                value.changed_at,
                value.expires_at
            ],
        )
        .map_err(unavailable)?;
    if changed == 0 {
        return Err(StorageError::new(
            StorageErrorCode::Conflict,
            "authorization revision is stale",
        ));
    }
    Ok(())
}

fn write_event(transaction: &Transaction<'_>, value: &EncodedEvent) -> Result<(), StorageError> {
    transaction
        .execute(
            "INSERT INTO journal_events(event_id, resource, sequence, payload, retain_until)\n\
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                value.event_id,
                value.resource,
                value.sequence,
                value.payload,
                value.retain_until
            ],
        )
        .map(|_| ())
        .map_err(unavailable)
}

fn write_delivery(
    transaction: &Transaction<'_>,
    value: &EncodedDelivery,
) -> Result<(), StorageError> {
    transaction
        .execute(
            "INSERT INTO target_deliveries(\n\
                 target_instance_id, target_revision, event_id, delivery_id, ordering_key,\n\
                 deadline, durability, payload\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                value.target_instance_id,
                value.target_revision,
                value.event_id,
                value.delivery_id,
                value.ordering_key,
                value.deadline,
                value.durability,
                value.payload
            ],
        )
        .map(|_| ())
        .map_err(unavailable)
}

fn write_record(transaction: &Transaction<'_>, value: &EncodedRecord) -> Result<(), StorageError> {
    let sequence: i64 = transaction
        .query_row(
            "UPDATE committed_source_streams SET high_water = high_water + 1
         WHERE durability = ?1 AND high_water < 9223372036854775807
         RETURNING high_water",
            [value.durability],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "INSERT INTO committed_records(
                 record_id, durability, committed_at, payload, retain_until, source_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                value.record_id,
                value.durability,
                value.committed_at,
                value.payload,
                value.retain_until,
                sequence,
            ],
        )
        .map(|_| ())
        .map_err(unavailable)
}
