mod configuration;
mod reads;

use std::sync::mpsc;

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use uob_application::{
    AtomicWriteOutcome, CommandAdmissionOutcome, CommandHistoryCursor, CommandHistoryQuery,
    CommandHistoryScope, CommittedRecord, CommittedRecordCursor, Page, RecordedDeliveryAttempt,
    RecoveryBatch, RetainedEventPage, ScheduledDelivery, SnapshotCursor, StorageError,
    StorageErrorCode, StorageRetentionStatus,
};
use uob_contracts::{
    Command, CommandResult, CommandSummary, ConfigurationObservation, EventEnvelope, ResourceRef,
    StationSnapshot, TriggerMessageClass, UtcTimestamp,
};

use crate::retention::SqliteRetentionPolicy;
use crate::{
    codec::{
        EncodedAuthorization, EncodedDelivery, EncodedDeliveryAttempt, EncodedEvent, EncodedRecord,
        EncodedWrite,
    },
    command, command_history,
    configuration::unavailable,
    delivery, recovery, retention, snapshots,
};

use reads::{read_events, read_records};

pub(crate) enum Request<C, E, D, R> {
    Drain(crate::drain::Operation, Reply<crate::drain::Outcome>),
    RemoteControl(
        crate::remote_control::Operation,
        Reply<crate::remote_control::Outcome>,
    ),
    Probe(Reply<()>),
    TransactionId(Reply<i32>),
    EventSequence(Reply<u64>),
    Write(EncodedWrite, Reply<AtomicWriteOutcome>),
    Snapshots(
        Option<String>,
        usize,
        Reply<Page<StationSnapshot, SnapshotCursor>>,
    ),
    StationSnapshot(String, Reply<Option<StationSnapshot>>),
    ScopedSnapshots(
        String,
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
    CommandCandidates(
        String,
        String,
        i64,
        Option<String>,
        usize,
        Reply<Vec<Command<C>>>,
    ),
    JournalEvent(String, String, String, Reply<Option<EventEnvelope<E>>>),
    CommandHistory(
        CommandHistoryQuery,
        CommandHistoryScope,
        Reply<Page<CommandSummary, CommandHistoryCursor>>,
    ),
    AppendConfigurationObservation(
        String,
        ConfigurationObservation,
        Reply<Option<CommandResult>>,
    ),
    TriggerPending(ResourceRef, TriggerMessageClass, UtcTimestamp, Reply<bool>),
    TriggerCandidates(Option<String>, usize, Reply<Vec<uob_contracts::RequestId>>),
    ReconcileTrigger(String, UtcTimestamp, Reply<Option<CommandResult>>),
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
    let mut drain = crate::drain::Drain::default();
    for request in requests {
        match request {
            Request::Drain(operation, reply) => respond(reply, drain.operation(&connection, operation)),
            Request::TransactionId(reply) => respond(reply, connection.query_row(
                "UPDATE transaction_id_counter SET value = value + 1 WHERE id = 1 AND value < 2147483647 RETURNING value",
                [], |row| row.get(0)).map_err(unavailable)),
            Request::EventSequence(reply) => respond(reply, connection.query_row(
                "UPDATE event_sequence_counter SET value = value + 1 WHERE id = 1 AND value < 9223372036854775807 RETURNING value",
                [], |row| row.get::<_, i64>(0)).map_err(unavailable).map(i64::cast_unsigned)),
            Request::RemoteControl(operation, reply) => respond(
                reply,
                apply_remote_control(&mut connection, &mut drain, &operation),
            ),
            Request::Probe(reply) => respond(reply, probe(&connection)),
            Request::Write(write, reply) => respond(
                reply,
                checked_write(&mut connection, &mut drain, retention_policy, write),
            ),
            Request::Snapshots(after, limit, reply) => {
                respond(reply, snapshots::read(&connection, after, limit));
            }
            Request::StationSnapshot(key, reply) => {
                respond(reply, snapshots::exact(&connection, &key));
            }
            Request::ScopedSnapshots(keys, after, limit, reply) => {
                respond(reply, snapshots::scoped(&connection, &keys, after, limit));
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
            Request::CommandCandidates(bridge, station, before, after, limit, reply) => {
                respond(reply, command::candidates(&connection, &bridge, &station, before, after.as_deref(), limit));
            }
            Request::JournalEvent(id, bridge, station, reply) => {
                respond(reply, command::journal_event(&connection, &id, &bridge, &station));
            }
            Request::CommandHistory(query, scope, reply) => {
                respond(reply, command_history::read(&connection, &query, &scope));
            }
            Request::AppendConfigurationObservation(write_id, observation, reply) => {
                respond(
                    reply,
                    append_configuration_observation(
                        &mut connection,
                        &mut drain,
                        &write_id,
                        observation,
                    ),
                );
            }
            Request::TriggerPending(station, class, now, reply) => {
                respond(reply, crate::trigger::pending(&connection, &station, class, now));
            }
            Request::TriggerCandidates(after, limit, reply) => {
                respond(reply, crate::trigger::candidates(&connection, after.as_deref(), limit));
            }
            Request::ReconcileTrigger(request_id, now, reply) => {
                respond(reply, drain.check_completion_write().and_then(|()| drain.changed())
                    .and_then(|()| crate::trigger::reconcile(&mut connection, &request_id, now)));
            }
            Request::PruneCommands(now, reply) => {
                respond(reply, command::prune::<C>(&mut connection, now));
            }
            Request::MaintainRetention(now, reply) => respond(
                reply,
                retention::maintain(&mut connection, retention_policy, now).map(|status| drain.admission_status(status)),
            ),
            Request::RetentionStatus(reply) => {
                respond(reply, retention::status(&connection, retention_policy).map(|status| drain.admission_status(status)));
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

fn probe(connection: &Connection) -> Result<(), StorageError> {
    connection
        .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
        .map(|_| ())
        .map_err(unavailable)
}

fn apply_remote_control(
    connection: &mut Connection,
    drain: &mut crate::drain::Drain,
    operation: &crate::remote_control::Operation,
) -> Result<crate::remote_control::Outcome, StorageError> {
    let guard = match operation {
        crate::remote_control::Operation::Read(_) => Ok(()),
        crate::remote_control::Operation::Reserve(_) => drain.check_remote_write(true),
        crate::remote_control::Operation::Response(..) => drain.check_remote_write(false),
    };
    guard.and_then(|()| crate::remote_control::apply(connection, operation))
}

fn checked_write(
    connection: &mut Connection,
    drain: &mut crate::drain::Drain,
    retention_policy: SqliteRetentionPolicy,
    write: EncodedWrite,
) -> Result<AtomicWriteOutcome, StorageError> {
    drain
        .check_write(&write)
        .and_then(|()| drain.changed())
        .and_then(|()| write_atomic(connection, retention_policy, write))
}

fn append_configuration_observation(
    connection: &mut Connection,
    drain: &mut crate::drain::Drain,
    write_id: &str,
    observation: ConfigurationObservation,
) -> Result<Option<CommandResult>, StorageError> {
    drain
        .check_completion_write()
        .and_then(|()| drain.changed())
        .and_then(|()| {
            configuration::append_configuration_observation(connection, write_id, observation)
        })
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
