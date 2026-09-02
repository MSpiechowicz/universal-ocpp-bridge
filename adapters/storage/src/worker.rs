use std::sync::mpsc;

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use uob_application::{
    AtomicWriteOutcome, CommandAdmissionOutcome, CommittedRecord, CommittedRecordCursor, Page,
    RETAINED_EVENT_CURSOR_PREFIX, RecordedDeliveryAttempt, RecoveryBatch, RetainedEventCursor,
    RetainedEventPage, ScheduledDelivery, SnapshotCursor, StorageError, StorageErrorCode,
    StorageRetentionStatus,
};
use uob_contracts::{Command, StationSnapshot};

use crate::retention::SqliteRetentionPolicy;
use crate::{
    codec::{
        self, EncodedAuthorization, EncodedDelivery, EncodedDeliveryAttempt, EncodedEvent,
        EncodedRecord, EncodedWrite,
    },
    command,
    configuration::unavailable,
    delivery, recovery, retention,
};

pub(crate) enum Request<C, E, D, R> {
    Write(EncodedWrite, Reply<AtomicWriteOutcome>),
    Snapshots(
        Option<String>,
        usize,
        Reply<Page<StationSnapshot, SnapshotCursor>>,
    ),
    Events(String, Option<i64>, usize, Reply<RetainedEventPage<E>>),
    Records(
        Option<i64>,
        usize,
        bool,
        Reply<Page<CommittedRecord<R>, CommittedRecordCursor>>,
    ),
    Recover(usize, Reply<RecoveryBatch<C, D>>),
    Command(String, Reply<Option<Command<C>>>),
    CommandResult(String, Reply<Option<uob_contracts::CommandResult>>),
    PruneCommands(i64, Reply<u64>),
    MaintainRetention(i64, Reply<StorageRetentionStatus>),
    RetentionStatus(Reply<StorageRetentionStatus>),
    PendingDeliveries(String, i64, String, usize, Reply<Vec<ScheduledDelivery<D>>>),
    RecordDeliveryAttempt(EncodedDeliveryAttempt, Reply<()>),
    DeliveryAttempts(String, usize, Reply<Vec<RecordedDeliveryAttempt>>),
}

pub(crate) type Reply<T> = oneshot::Sender<Result<T, StorageError>>;

pub(crate) fn run<C, E, D, R>(
    mut connection: Connection,
    requests: mpsc::Receiver<Request<C, E, D, R>>,
    retention_policy: SqliteRetentionPolicy,
) where
    C: DeserializeOwned + Serialize,
    E: DeserializeOwned,
    D: DeserializeOwned,
    R: DeserializeOwned,
{
    for request in requests {
        match request {
            Request::Write(write, reply) => respond(
                reply,
                write_atomic(&mut connection, retention_policy, write),
            ),
            Request::Snapshots(after, limit, reply) => {
                respond(reply, read_snapshots(&connection, after, limit));
            }
            Request::Events(resource, after, limit, reply) => {
                respond(reply, read_events(&connection, &resource, after, limit));
            }
            Request::Records(after, limit, telemetry, reply) => {
                respond(reply, read_records(&connection, after, limit, telemetry));
            }
            Request::Recover(limit, reply) => {
                respond(reply, recovery::recover(&connection, limit));
            }
            Request::Command(request_id, reply) => {
                respond(reply, recovery::command(&connection, &request_id));
            }
            Request::CommandResult(request_id, reply) => {
                respond(reply, recovery::command_result(&connection, &request_id));
            }
            Request::PruneCommands(now, reply) => {
                respond(reply, command::prune::<C>(&mut connection, now));
            }
            Request::MaintainRetention(now, reply) => respond(
                reply,
                retention::maintain(&mut connection, retention_policy, now),
            ),
            Request::RetentionStatus(reply) => {
                respond(reply, retention::status(&connection, retention_policy));
            }
            Request::PendingDeliveries(target, revision, ready_at, limit, reply) => respond(
                reply,
                delivery::read_pending(&connection, &target, revision, &ready_at, limit),
            ),
            Request::RecordDeliveryAttempt(attempt, reply) => {
                respond(reply, delivery::record_attempt(&mut connection, &attempt));
            }
            Request::DeliveryAttempts(delivery_id, limit, reply) => respond(
                reply,
                delivery::read_attempts(&connection, &delivery_id, limit),
            ),
        }
    }
}

fn respond<T>(reply: Reply<T>, result: Result<T, StorageError>) {
    let _ignored = reply.send(result);
}

fn write_atomic(
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
    if let Some((station, payload)) = write.snapshot {
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
        transaction
            .execute(
                "INSERT INTO command_results(request_id, payload) VALUES (?1, ?2)\n\
                 ON CONFLICT(request_id) DO UPDATE SET payload = excluded.payload",
                params![result.request_id, result.payload],
            )
            .map_err(unavailable)?;
        transaction
            .execute(
                "UPDATE commands SET unresolved = ?2 WHERE request_id = ?1",
                params![result.request_id, i64::from(result.unresolved)],
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
            "INSERT INTO authorization_changes(reference, resource, state, revision, changed_at)\n\
             VALUES (?1, ?2, ?3, ?4, ?5)\n\
             ON CONFLICT(reference) DO UPDATE SET resource = excluded.resource,\n\
                 state = excluded.state, revision = excluded.revision,\n\
                 changed_at = excluded.changed_at\n\
             WHERE excluded.revision >= authorization_changes.revision",
            params![
                value.reference,
                value.resource,
                value.state,
                value.revision,
                value.changed_at
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
    transaction
        .execute(
            "INSERT INTO committed_records(\n\
                 record_id, durability, committed_at, payload, retain_until\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                value.record_id,
                value.durability,
                value.committed_at,
                value.payload,
                value.retain_until
            ],
        )
        .map(|_| ())
        .map_err(unavailable)
}

fn read_snapshots(
    connection: &Connection,
    after: Option<String>,
    limit: usize,
) -> Result<Page<StationSnapshot, SnapshotCursor>, StorageError> {
    let after = after.unwrap_or_default();
    let mut statement = connection
        .prepare(
            "SELECT station_key, payload FROM station_snapshots\n\
             WHERE station_key > ?1 ORDER BY station_key LIMIT ?2",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![after, limit_plus_one(limit)?], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut values = collect_rows(rows)?;
    let has_more = values.len() > limit;
    values.truncate(limit);
    let next_cursor = has_more
        .then(|| {
            values
                .last()
                .map(|value| SnapshotCursor::new(value.0.clone()))
        })
        .flatten()
        .transpose()?;
    let items = values
        .into_iter()
        .map(|(_, payload)| codec::decode_snapshot(&payload))
        .collect::<Result<_, _>>()?;
    Ok(Page { items, next_cursor })
}

fn read_events<E: DeserializeOwned>(
    connection: &Connection,
    resource: &str,
    after: Option<i64>,
    limit: usize,
) -> Result<RetainedEventPage<E>, StorageError> {
    if let Some(cursor) = after {
        let retained = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM journal_events WHERE resource = ?1 AND row_id = ?2)",
                params![resource, cursor],
                |row| row.get::<_, bool>(0),
            )
            .map_err(unavailable)?;
        if !retained {
            return Err(StorageError::new(
                StorageErrorCode::CursorExpired,
                "durable event cursor expired; fetch a fresh snapshot",
            ));
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT row_id, payload FROM journal_events WHERE resource = ?1 AND row_id > ?2\n\
             ORDER BY row_id LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![resource, after.unwrap_or(0), limit_plus_one(limit)?],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(unavailable)?;
    let mut values = collect_rows(rows)?;
    let has_more = values.len() > limit;
    values.truncate(limit);
    let resume_position = values.last().map_or(after, |value| Some(value.0));
    let resume_cursor = resume_position
        .map(|position| {
            RetainedEventCursor::new(format!("{RETAINED_EVENT_CURSOR_PREFIX}{position}"))
        })
        .transpose()?;
    let events = values
        .into_iter()
        .map(|(_, payload)| codec::decode_event(&payload))
        .collect::<Result<_, _>>()?;
    Ok(RetainedEventPage {
        events,
        resume_cursor,
        has_more,
    })
}

fn read_records<R: DeserializeOwned>(
    connection: &Connection,
    after: Option<i64>,
    limit: usize,
    include_telemetry: bool,
) -> Result<Page<CommittedRecord<R>, CommittedRecordCursor>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT row_id, record_id, durability, committed_at, payload\n\
             FROM committed_records WHERE row_id > ?1 AND (?2 OR durability = 0)\n\
             ORDER BY row_id LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![
                after.unwrap_or(0),
                include_telemetry,
                limit_plus_one(limit)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(unavailable)?;
    let mut values = collect_rows(rows)?;
    let has_more = values.len() > limit;
    values.truncate(limit);
    let next_cursor = has_more
        .then(|| {
            values
                .last()
                .map(|value| CommittedRecordCursor::new(value.0.to_string()))
        })
        .flatten()
        .transpose()?;
    let items = values
        .into_iter()
        .map(|(_, id, durability, at, payload)| codec::decode_record(id, durability, &at, &payload))
        .collect::<Result<_, _>>()?;
    Ok(Page { items, next_cursor })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, StorageError> {
    rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
}

fn limit_plus_one(limit: usize) -> Result<i64, StorageError> {
    limit_i64(limit.saturating_add(1))
}
fn limit_i64(limit: usize) -> Result<i64, StorageError> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "page limit exceeds SQLite integer range",
        )
    })
}
