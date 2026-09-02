use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::pin,
    sync::{Arc, Barrier},
    task::{Context, Poll, Wake, Waker},
};

use rusqlite::{Connection, params};
use time::{Date, Duration, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, CommandAdmissionOutcome, OperationalStore, StorageErrorCode,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, Command, CommandLifecycle, CommandOperation,
    CommandRequest, CommandResult, ContractVersion, ExternalCommand, PrincipalId, RequestId,
    ResourceRef, StationId, TransactionId, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-command-dedup-{}.sqlite3", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _database = std::fs::remove_file(&self.0);
        let _wal = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _shared_memory = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

#[test]
fn concurrent_and_restart_retries_dispatch_once_and_return_known_result() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    let barrier = Arc::new(Barrier::new(3));
    let handles = [0, 1].map(|hour| {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            block_on(store.write_atomic(command_write(command(
                "request-concurrent",
                "station-a",
                start(None),
                hour,
            ))))
            .expect("concurrent admission")
            .command
            .expect("command outcome")
        })
    });
    barrier.wait();
    let outcomes = handles.map(|handle| handle.join().expect("admission thread"));
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CommandAdmissionOutcome::Admitted))
            .count(),
        1,
        "only a newly admitted command may be dispatched"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CommandAdmissionOutcome::Duplicate { .. }))
            .count(),
        1
    );

    let original = command("request-concurrent", "station-a", start(None), 0);
    let known_result = result(&original, accepted(), 2);
    let mut result_write = AtomicStoreWrite::empty();
    result_write.command_result = Some(known_result.clone());
    block_on(store.write_atomic(result_write)).expect("persist command result");
    drop(store);

    let reopened = Store::open(database.path(), 8).expect("reopen SQLite store");
    let retry = block_on(reopened.write_atomic(command_write(command(
        "request-concurrent",
        "station-a",
        start(None),
        3,
    ))))
    .expect("restart retry");
    assert_eq!(
        retry.command,
        Some(CommandAdmissionOutcome::Duplicate {
            result: Some(Box::new(known_result))
        })
    );
}

#[test]
fn reused_ids_with_changed_station_action_or_parameters_return_safe_conflicts() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    let cases = [
        (
            command("request-station", "station-a", start(None), 0),
            command("request-station", "station-b", start(None), 1),
        ),
        (
            command("request-action", "station-a", start(None), 0),
            command(
                "request-action",
                "station-a",
                CommandOperation::Stop {
                    transaction_id: text(TransactionId::new, "transaction-1"),
                },
                1,
            ),
        ),
        (
            command("request-parameters", "station-a", start(None), 0),
            command(
                "request-parameters",
                "station-a",
                start(Some("local-auth-2")),
                1,
            ),
        ),
    ];

    for (original, conflicting) in cases {
        block_on(store.write_atomic(command_write(original))).expect("seed command");
        let error = block_on(store.write_atomic(command_write(conflicting)))
            .expect_err("changed command content must conflict");
        assert_eq!(error.code(), StorageErrorCode::Conflict);
        assert_eq!(
            error.detail(),
            "request ID is already associated with another command"
        );
        assert!(std::error::Error::source(&error).is_none());
    }
}

#[test]
fn seven_day_pruning_removes_only_resolved_command_identities() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    let resolved = command("request-resolved", "station-a", start(None), 0);
    let pending = command("request-pending", "station-a", start(None), 0);
    let uncertain = command("request-uncertain", "station-a", start(None), 0);

    for command in [&resolved, &pending, &uncertain] {
        block_on(store.write_atomic(command_write(command.clone()))).expect("admit command");
    }
    for result in [
        result(&resolved, accepted(), 1),
        result(
            &uncertain,
            CommandLifecycle::TransmissionUncertain {
                detail: "connection closed after transmission".to_owned(),
            },
            1,
        ),
    ] {
        let mut write = AtomicStoreWrite::empty();
        write.command_result = Some(result);
        block_on(store.write_atomic(write)).expect("persist result");
    }

    assert_eq!(
        block_on(store.prune_command_deduplication(timestamp(167)))
            .expect("prune before retention boundary"),
        0
    );
    assert_eq!(
        block_on(store.prune_command_deduplication(timestamp(168)))
            .expect("prune at retention boundary"),
        1
    );
    assert!(
        block_on(store.command_by_request_id(text(RequestId::new, "request-resolved")))
            .expect("resolved lookup")
            .is_none()
    );
    for request_id in ["request-pending", "request-uncertain"] {
        assert!(
            block_on(store.command_by_request_id(text(RequestId::new, request_id)))
                .expect("unresolved lookup")
                .is_some(),
            "unresolved command {request_id} must survive retention pruning"
        );
    }
}

#[test]
fn legacy_command_rows_are_backfilled_before_retention_pruning() {
    let database = TestDatabase::new();
    let command = command("request-legacy", "station-a", start(None), 0);
    let result = result(&command, accepted(), 1);
    let connection = Connection::open(database.path()).expect("open legacy database");
    connection
        .execute_batch(
            "CREATE TABLE commands (request_id TEXT PRIMARY KEY, payload TEXT NOT NULL);\n\
             CREATE TABLE command_results (request_id TEXT PRIMARY KEY, payload TEXT NOT NULL);\n\
             PRAGMA user_version = 2;",
        )
        .expect("create legacy command schema");
    connection
        .execute(
            "INSERT INTO commands(request_id, payload) VALUES (?1, ?2)",
            params![
                command.request_id.as_str(),
                serde_json::to_string(&command).expect("encode legacy command")
            ],
        )
        .expect("insert legacy command");
    connection
        .execute(
            "INSERT INTO command_results(request_id, payload) VALUES (?1, ?2)",
            params![
                command.request_id.as_str(),
                serde_json::to_string(&result).expect("encode legacy result")
            ],
        )
        .expect("insert legacy result");
    drop(connection);

    let store = Store::open(database.path(), 8).expect("migrate legacy store");
    assert_eq!(
        block_on(store.prune_command_deduplication(timestamp(168)))
            .expect("backfill and prune legacy command"),
        1
    );
    assert!(
        block_on(store.command_by_request_id(text(RequestId::new, "request-legacy")))
            .expect("legacy command lookup")
            .is_none()
    );
}

fn command(
    request_id: &str,
    station_id: &str,
    operation: CommandOperation<String>,
    admitted_hour: i64,
) -> Command<String> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: text(RequestId::new, request_id),
            correlation_id: None,
            resource: resource(station_id),
            operation,
            expires_at: timestamp(240),
        },
        AuthenticatedCommandOrigin::Management {
            principal_id: text(PrincipalId::new, "operator-1"),
        },
    )
    .admit(timestamp(admitted_hour))
}

fn start(authorization_reference: Option<&str>) -> CommandOperation<String> {
    CommandOperation::Start {
        authorization_reference: authorization_reference.map(str::to_owned),
    }
}

fn accepted() -> CommandLifecycle {
    CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    }
}

fn result(command: &Command<String>, lifecycle: CommandLifecycle, hour: i64) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at: timestamp(hour),
        observed_effects: Vec::new(),
    }
}

fn command_write(command: Command<String>) -> AtomicStoreWrite<String, String, String, String> {
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command);
    write
}

fn resource(station_id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-test"),
        station_id: text(StationId::new, station_id),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp(hour: i64) -> UtcTimestamp {
    let base = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::September, 1).expect("test date"),
        Time::MIDNIGHT,
    )
    .assume_offset(UtcOffset::UTC);
    UtcTimestamp::new(base + Duration::hours(hour))
}

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid test identity")
}

fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}
