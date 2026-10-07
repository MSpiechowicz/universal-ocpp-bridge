mod command_requests;
mod configuration;
mod reads;
mod recovery_requests;
mod write;
use write::write_atomic;

use std::{collections::hash_map::RandomState, sync::mpsc};

use rusqlite::Connection;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use uob_application::{
    AtomicWriteOutcome, CommandHistoryCursor, CommandHistoryQuery, CommandHistoryScope,
    CommittedRecordChunkQuery, CommittedRecordChunkResult, CommittedRecordPage,
    CommittedRecordQuery, Page, RecordedDeliveryAttempt, RecoveryBatch, RecoveryQuery,
    RetainedEventPage, RuntimeReservation, ScheduledDelivery, SnapshotCursor, StorageError,
    StorageRetentionStatus,
};
use uob_contracts::{
    Command, CommandResult, CommandSummary, ConfigurationObservation, EventEnvelope, ResourceRef,
    StationSnapshot, TriggerMessageClass, TriggerMessageClass201, UtcTimestamp,
};

use crate::retention::SqliteRetentionPolicy;
use crate::{
    codec::{EncodedDeliveryAttempt, EncodedWrite},
    configuration::unavailable,
    delivery, retention, snapshots,
};

use reads::{read_events, read_record_chunk, read_records};

pub(crate) enum Request<C, E, D> {
    Drain(crate::drain::Operation, Reply<crate::drain::Outcome>),
    RemoteControl(
        crate::remote_control::Operation,
        Reply<crate::remote_control::Outcome>,
    ),
    Probe(Reply<()>),
    ChargingProfileOwnership(ResourceRef, Reply<uob_application::ProfileOwnership201>),
    ChargingProfileOwners(ResourceRef, Reply<uob_application::ProfileOwners201>),
    InterruptChargingProfiles(Reply<()>),
    Reservations16(
        ResourceRef,
        Reply<Vec<uob_application::ReservationRecord16>>,
    ),
    MaintainReservations16(UtcTimestamp, bool, Reply<()>),
    Reservations201(
        ResourceRef,
        Reply<Vec<uob_application::ReservationRecord201>>,
    ),
    MaintainReservations201(UtcTimestamp, bool, Reply<()>),
    FirmwareJobs16(
        ResourceRef,
        Reply<Vec<uob_application::FirmwareJobRecord16>>,
    ),
    MaintainFirmware16(UtcTimestamp, bool, Reply<()>),
    FirmwareJobs201(
        ResourceRef,
        Reply<Vec<uob_application::FirmwareJobRecord201>>,
    ),
    MaintainFirmware201(UtcTimestamp, bool, Reply<()>),
    DiagnosticsJobs16(
        ResourceRef,
        Reply<Vec<uob_application::DiagnosticsJobRecord16>>,
    ),
    BindDiagnosticsUpload16(uob_contracts::RequestId, String, Reply<()>),
    MaintainDiagnostics16(UtcTimestamp, bool, Reply<()>),
    DiagnosticsJobs201(
        ResourceRef,
        Reply<Vec<uob_application::DiagnosticsJobRecord201>>,
    ),
    BindDiagnosticsUpload201(uob_contracts::RequestId, String, Reply<()>),
    MaintainDiagnostics201(UtcTimestamp, bool, Reply<()>),
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
        CommittedRecordQuery,
        RuntimeReservation,
        Reply<CommittedRecordPage>,
    ),
    RecordChunk(
        CommittedRecordChunkQuery,
        RuntimeReservation,
        Reply<CommittedRecordChunkResult>,
    ),
    Recover(RecoveryQuery, Reply<RecoveryBatch<C, D>>),
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
    TriggerPending201(
        ResourceRef,
        TriggerMessageClass201,
        UtcTimestamp,
        Reply<bool>,
    ),
    TriggerCandidates(Option<String>, usize, Reply<Vec<uob_contracts::RequestId>>),
    ReconcileTrigger(String, UtcTimestamp, Reply<Option<CommandResult>>),
    DeviceReport(
        String,
        uob_contracts::DeviceModelResult201,
        Option<uob_contracts::CommandLifecycle>,
        UtcTimestamp,
        Reply<Option<CommandResult>>,
    ),
    InterruptDeviceReports(Reply<()>),
    ChargingProfilesReport(
        String,
        Box<uob_contracts::ChargingProfilesResult201>,
        Option<uob_contracts::CommandLifecycle>,
        UtcTimestamp,
        Reply<Option<CommandResult>>,
    ),
    PruneCommands(i64, Reply<u64>),
    MaintainRetention(i64, Reply<StorageRetentionStatus>),
    RetentionStatus(Reply<StorageRetentionStatus>),
    PendingDeliveries(String, i64, String, usize, Reply<Vec<ScheduledDelivery<D>>>),
    RecordDeliveryAttempt(EncodedDeliveryAttempt, Reply<()>),
    DeliveryAttempts(String, usize, Reply<Vec<RecordedDeliveryAttempt>>),
    TargetBacklog(Reply<Vec<uob_application::TargetBacklogFact>>),
    PendingTargetDispositions(Reply<Vec<uob_application::DeliveryDispositionRecord>>),
    AuthorizeTargetDisposition(
        uob_application::DeliveryDispositionRequest,
        Reply<uob_application::DeliveryDispositionRecord>,
    ),
    SettleTargetDispositions(
        Option<uob_application::TargetDeliveryDestination>,
        UtcTimestamp,
        Reply<Vec<uob_application::DeliveryDispositionRecord>>,
    ),
}

pub(crate) type Reply<T> = oneshot::Sender<Result<T, StorageError>>;

pub(crate) fn run<C, E, D>(
    mut connection: Connection,
    requests: mpsc::Receiver<Request<C, E, D>>,
    retention_policy: SqliteRetentionPolicy,
) where
    C: DeserializeOwned + Serialize,
    E: DeserializeOwned,
    D: DeserializeOwned,
{
    let mut drain = crate::drain::Drain::default();
    let token_key = RandomState::new();
    for request in requests {
        handle_request(
            &mut connection,
            &mut drain,
            &token_key,
            retention_policy,
            request,
        );
    }
}

#[allow(clippy::too_many_lines)] // One exhaustive dispatch keeps every request on the writer thread.
fn handle_request<C, E, D>(
    connection: &mut Connection,
    drain: &mut crate::drain::Drain,
    token_key: &RandomState,
    retention_policy: SqliteRetentionPolicy,
    request: Request<C, E, D>,
) where
    C: DeserializeOwned + Serialize,
    E: DeserializeOwned,
    D: DeserializeOwned,
{
    match request {
        Request::Drain(operation, reply) => respond(reply, drain.operation(connection, operation)),
        Request::Reservations16(station, reply) => {
            respond(reply, crate::reservation16::read(connection, &station));
        }
        Request::MaintainReservations16(now, startup, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::reservation16::maintain(connection, now, startup)),
        ),
        Request::Reservations201(station, reply) => {
            respond(reply, crate::reservation201::read(connection, &station));
        }
        Request::MaintainReservations201(now, startup, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::reservation201::maintain(connection, now, startup)),
        ),
        Request::FirmwareJobs16(station, reply) => {
            respond(reply, crate::firmware16::read(connection, &station));
        }
        Request::MaintainFirmware16(now, startup, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::firmware16::maintain(connection, now, startup)),
        ),
        Request::FirmwareJobs201(station, reply) => {
            respond(reply, crate::firmware201::read(connection, &station));
        }
        Request::MaintainFirmware201(now, startup, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::firmware201::maintain(connection, now, startup)),
        ),
        Request::DiagnosticsJobs16(station, reply) => {
            respond(reply, crate::diagnostics16::read(connection, &station));
        }
        Request::BindDiagnosticsUpload16(request_id, upload_id, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| {
                    crate::diagnostics16::bind_upload(connection, &request_id, upload_id)
                }),
        ),
        Request::MaintainDiagnostics16(now, startup, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::diagnostics16::maintain(connection, now, startup)),
        ),
        Request::DiagnosticsJobs201(station, reply) => {
            respond(reply, crate::diagnostics201::read(connection, &station));
        }
        Request::BindDiagnosticsUpload201(request_id, upload_id, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| {
                    crate::diagnostics201::bind_upload(connection, &request_id, upload_id)
                }),
        ),
        Request::MaintainDiagnostics201(now, startup, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::diagnostics201::maintain(connection, now, startup)),
        ),
        Request::ChargingProfileOwnership(station, reply) => {
            respond(
                reply,
                crate::charging_profile201::read(connection, &station),
            );
        }
        Request::ChargingProfileOwners(station, reply) => {
            respond(
                reply,
                crate::charging_profile201::owners(connection, &station),
            );
        }
        Request::InterruptChargingProfiles(reply) => {
            respond(
                reply,
                drain
                    .check_completion_write()
                    .and_then(|()| drain.changed())
                    .and_then(|()| crate::charging_profile201::recover(connection)),
            );
        }
        Request::TransactionId(reply) => respond(reply, next_transaction_id(connection)),
        Request::EventSequence(reply) => respond(reply, next_event_sequence(connection)),
        Request::RemoteControl(operation, reply) => {
            respond(reply, apply_remote_control(connection, drain, &operation));
        }
        Request::Probe(reply) => respond(reply, probe(connection)),
        Request::Write(write, reply) => respond(
            reply,
            checked_write(connection, drain, retention_policy, write),
        ),
        Request::Snapshots(after, limit, reply) => {
            respond(reply, snapshots::read(connection, after, limit));
        }
        Request::StationSnapshot(key, reply) => {
            respond(reply, snapshots::exact(connection, &key));
        }
        Request::ScopedSnapshots(keys, after, limit, reply) => {
            respond(reply, snapshots::scoped(connection, &keys, after, limit));
        }
        Request::Events(resource, after, limit, reply) => {
            respond(reply, read_events(connection, &resource, after, limit));
        }
        Request::Records(query, reservation, reply) => {
            respond(
                reply,
                read_records(connection, &query, reservation, token_key),
            );
        }
        Request::RecordChunk(query, reservation, reply) => {
            respond(
                reply,
                read_record_chunk(connection, &query, reservation, token_key),
            );
        }
        recovery_request @ (Request::Recover(..)
        | Request::Command(..)
        | Request::CommandResult(..)) => {
            recovery_requests::handle(connection, recovery_request);
        }
        command_request @ (Request::CommandCandidates(..)
        | Request::JournalEvent(..)
        | Request::CommandHistory(..)
        | Request::AppendConfigurationObservation(..)
        | Request::TriggerPending(..)
        | Request::TriggerPending201(..)
        | Request::TriggerCandidates(..)
        | Request::ReconcileTrigger(..)
        | Request::DeviceReport(..)
        | Request::InterruptDeviceReports(..)
        | Request::ChargingProfilesReport(..)
        | Request::PruneCommands(..)) => {
            command_requests::handle(connection, drain, command_request);
        }
        Request::MaintainRetention(now, reply) => respond(
            reply,
            retention::maintain(connection, retention_policy, now)
                .map(|status| drain.admission_status(status)),
        ),
        Request::RetentionStatus(reply) => {
            respond(
                reply,
                retention::status(connection, retention_policy)
                    .map(|status| drain.admission_status(status)),
            );
        }
        Request::PendingDeliveries(target, revision, ready_at, limit, reply) => respond(
            reply,
            delivery::read_pending(connection, &target, revision, &ready_at, limit),
        ),
        Request::RecordDeliveryAttempt(attempt, reply) => {
            respond(reply, delivery::record_attempt(connection, &attempt));
        }
        Request::DeliveryAttempts(delivery_id, limit, reply) => respond(
            reply,
            delivery::read_attempts(connection, &delivery_id, limit),
        ),
        Request::TargetBacklog(reply) => {
            respond(reply, crate::target_disposition::backlog(connection));
        }
        Request::PendingTargetDispositions(reply) => {
            respond(reply, crate::target_disposition::pending(connection));
        }
        Request::AuthorizeTargetDisposition(request, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| crate::target_disposition::authorize(connection, &request)),
        ),
        Request::SettleTargetDispositions(selected, settled_at, reply) => respond(
            reply,
            drain
                .check_completion_write()
                .and_then(|()| drain.changed())
                .and_then(|()| {
                    crate::target_disposition::settle(connection, selected.as_ref(), settled_at)
                }),
        ),
    }
}

fn next_transaction_id(connection: &Connection) -> Result<i32, StorageError> {
    connection
        .query_row(
            "UPDATE transaction_id_counter SET value = value + 1 WHERE id = 1 AND value < 2147483647 RETURNING value",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

fn next_event_sequence(connection: &Connection) -> Result<u64, StorageError> {
    connection
        .query_row(
            "UPDATE event_sequence_counter SET value = value + 1 WHERE id = 1 AND value < 9223372036854775807 RETURNING value",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(unavailable)
        .map(i64::cast_unsigned)
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
