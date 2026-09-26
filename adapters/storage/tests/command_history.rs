use std::{
    future::Future,
    path::PathBuf,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use time::{Date, Duration, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, CommandHistoryCursor, CommandHistoryQuery, CommandHistoryScope,
    OperationalStore, PageLimit, StorageErrorCode,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, CanonicalConnectorId, CanonicalResource, Command,
    CommandLifecycle, CommandOperation, CommandRequest, CommandResult, ContractVersion,
    ExternalCommand, PrincipalId, RequestId, ResourceRef, StationId, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;

struct TestDatabase(PathBuf);
impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-command-history-{}.sqlite3", Uuid::new_v4())))
    }
}
impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

#[test]
fn scoped_pages_ignore_interleaved_foreign_resources_and_resume_after_restart() {
    let database = TestDatabase::new();
    let store = Store::open(&database.0, 8).expect("open store");
    let station = resource("station-a", None);
    let child = connector(1);
    let allowed = CommandHistoryScope {
        descendants: false,
        station_only: false,
        resources: vec![child.clone()],
    };
    let rows = [
        ("older-allowed", "station-a", Some(1), 1),
        ("foreign-station", "station-b", Some(1), 2),
        ("foreign-child", "station-a", Some(2), 3),
        ("newer-allowed", "station-a", Some(1), 4),
        ("newest-foreign", "station-a", None, 5),
    ];
    for (id, station_id, connector_id, hour) in rows {
        let command = command(id, station_id, connector_id, hour);
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(command.clone());
        block_on(store.write_atomic(write)).expect("admit command");
        let mut completion = AtomicStoreWrite::empty();
        completion.command_result = Some(result(&command, hour));
        block_on(store.write_atomic(completion)).expect("write result");
    }
    let query = |after| CommandHistoryQuery {
        station: station.clone(),
        after,
        limit: PageLimit::new(1).unwrap(),
    };
    let first =
        block_on(store.read_command_history(query(None), allowed.clone())).expect("first page");
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.request_id.as_str())
            .collect::<Vec<_>>(),
        ["newer-allowed"]
    );
    assert!(first.items[0].lifecycle.is_some());
    assert_eq!(first.items[0].resource.resource, Some(child));
    let cursor = first.next_cursor.expect("next page across foreign records");
    drop(store);
    let reopened = Store::open(&database.0, 8).expect("reopen store");
    let second =
        block_on(reopened.read_command_history(query(Some(cursor.clone())), allowed.clone()))
            .expect("resume page");
    assert_eq!(
        second
            .items
            .iter()
            .map(|item| item.request_id.as_str())
            .collect::<Vec<_>>(),
        ["older-allowed"]
    );
    assert!(second.next_cursor.is_none());
    assert_eq!(
        block_on(reopened.read_command_history(
            query(Some(cursor)),
            CommandHistoryScope {
                descendants: true,
                station_only: false,
                resources: vec![],
            }
        ))
        .unwrap_err()
        .code(),
        StorageErrorCode::InvalidRequest,
        "cursor must not cross read-grant scope"
    );
}

#[test]
fn history_is_sanitized_and_rejects_malformed_cross_station_and_pruned_positions() {
    let database = TestDatabase::new();
    let store = Store::open(&database.0, 8).expect("open store");
    let old = command("old", "station-a", None, 0);
    let newer = command("newer", "station-a", None, 1);
    for command in [&old, &newer] {
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(command.clone());
        block_on(store.write_atomic(write)).expect("admit");
        let mut result_write = AtomicStoreWrite::empty();
        result_write.command_result = Some(result(command, 2));
        block_on(store.write_atomic(result_write)).expect("complete");
    }
    let pending = command("pending", "station-a", None, -1);
    let mut admission = AtomicStoreWrite::empty();
    admission.command = Some(pending);
    block_on(store.write_atomic(admission)).expect("admit unresolved command");
    let scope = CommandHistoryScope {
        descendants: false,
        station_only: true,
        resources: vec![],
    };
    let query = |station, after| CommandHistoryQuery {
        station,
        after,
        limit: PageLimit::new(1).unwrap(),
    };
    let station = resource("station-a", None);
    let first = block_on(store.read_command_history(query(station.clone(), None), scope.clone()))
        .expect("first page");
    let serialized = serde_json::to_string(&first.items[0]).unwrap();
    assert!(!serialized.contains("private-auth-reference"));
    assert!(!serialized.contains("operator-private"));
    assert!(!serialized.contains("authorization_reference"));
    assert!(!serialized.contains("configuration"));
    let cursor = first.next_cursor.expect("cursor");
    let bad = CommandHistoryCursor::new("uob:command:garbage").unwrap();
    assert_eq!(
        block_on(store.read_command_history(query(station.clone(), Some(bad)), scope.clone()))
            .unwrap_err()
            .code(),
        StorageErrorCode::InvalidRequest
    );
    assert_eq!(
        block_on(store.read_command_history(
            query(resource("station-b", None), Some(cursor.clone())),
            scope.clone()
        ))
        .unwrap_err()
        .code(),
        StorageErrorCode::InvalidRequest
    );
    assert_eq!(
        block_on(store.prune_command_deduplication(timestamp(169)))
            .expect("prune expired resolved entries"),
        2
    );
    assert_eq!(
        block_on(store.read_command_history(query(station, Some(cursor)), scope))
            .unwrap_err()
            .code(),
        StorageErrorCode::CursorExpired
    );
    let retained = block_on(store.read_command_history(
        query(resource("station-a", None), None),
        CommandHistoryScope {
            descendants: false,
            station_only: true,
            resources: vec![],
        },
    ))
    .expect("unresolved identity remains retained");
    assert_eq!(retained.items[0].request_id.as_str(), "pending");
    assert!(retained.items[0].lifecycle.is_none());
}

#[test]
fn large_request_ids_paginate_at_equal_times_without_exposing_the_id_in_cursor() {
    let database = TestDatabase::new();
    let store = Store::open(&database.0, 8).expect("open store");
    let older_id = format!("a{}", "x".repeat(5_000));
    let anchor_id = format!("b{}", "y".repeat(5_000));
    let station = resource("station-a", None);
    let scope = CommandHistoryScope {
        descendants: false,
        station_only: true,
        resources: vec![],
    };
    for id in [&older_id, &anchor_id] {
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(command(id, "station-a", None, 1));
        block_on(store.write_atomic(write)).expect("admit long request ID");
    }
    let query = |after| CommandHistoryQuery {
        station: station.clone(),
        after,
        limit: PageLimit::new(1).unwrap(),
    };
    let first = block_on(store.read_command_history(query(None), scope.clone()))
        .expect("first page with large anchor");
    assert_eq!(first.items[0].request_id.as_str(), anchor_id);
    let cursor = first.next_cursor.expect("large anchor has next page");
    assert!(cursor.as_str().len() < 256, "cursor must be bounded");
    assert!(!cursor.as_str().contains(&anchor_id));

    drop(store);
    let reopened = Store::open(&database.0, 8).expect("reopen store");
    let second = block_on(reopened.read_command_history(query(Some(cursor)), scope))
        .expect("resume long-ID history");
    assert_eq!(second.items[0].request_id.as_str(), older_id);
    assert!(second.next_cursor.is_none());
}

#[test]
fn cursor_expires_when_anchor_rowid_is_reused_by_a_different_command() {
    let database = TestDatabase::new();
    let store = Store::open(&database.0, 8).expect("open store");
    let station = resource("station-a", None);
    let scope = CommandHistoryScope {
        descendants: false,
        station_only: true,
        resources: vec![],
    };
    for id in ["a-older", "z-newer"] {
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(command(id, "station-a", None, 1));
        block_on(store.write_atomic(write)).expect("admit");
    }
    let query = |after| CommandHistoryQuery {
        station: station.clone(),
        after,
        limit: PageLimit::new(1).unwrap(),
    };
    let cursor = block_on(store.read_command_history(query(None), scope.clone()))
        .expect("first page")
        .next_cursor
        .expect("cursor");
    drop(store);

    let connection = rusqlite::Connection::open(&database.0).expect("open database");
    let old_rowid: i64 = connection
        .query_row(
            "SELECT rowid FROM commands WHERE request_id = 'z-newer'",
            [],
            |row| row.get(0),
        )
        .expect("original rowid");
    connection
        .execute("DELETE FROM commands WHERE request_id = 'z-newer'", [])
        .expect("delete anchor");
    drop(connection);

    let reopened = Store::open(&database.0, 8).expect("reopen store");
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command("replacement", "station-a", None, 1));
    block_on(reopened.write_atomic(write)).expect("admit replacement");
    let connection = rusqlite::Connection::open(&database.0).expect("open database");
    let new_rowid: i64 = connection
        .query_row(
            "SELECT rowid FROM commands WHERE request_id = 'replacement'",
            [],
            |row| row.get(0),
        )
        .expect("replacement rowid");
    assert_eq!(old_rowid, new_rowid, "exercise actual SQLite rowid reuse");
    assert_eq!(
        block_on(reopened.read_command_history(query(Some(cursor)), scope))
            .unwrap_err()
            .code(),
        StorageErrorCode::CursorExpired
    );
}

#[test]
fn cursor_expires_if_anchor_is_reordered_or_replaced_in_place() {
    for change in [
        "UPDATE commands SET admitted_at = admitted_at + 1 WHERE request_id = 'z-newer'",
        "UPDATE commands SET payload = replace(payload, 'operator-private', 'operator-other') WHERE request_id = 'z-newer'",
    ] {
        let database = TestDatabase::new();
        let store = Store::open(&database.0, 8).expect("open store");
        let station = resource("station-a", None);
        let scope = CommandHistoryScope {
            descendants: false,
            station_only: true,
            resources: vec![],
        };
        for id in ["a-older", "z-newer"] {
            let mut write = AtomicStoreWrite::empty();
            write.command = Some(command(id, "station-a", None, 1));
            block_on(store.write_atomic(write)).expect("admit");
        }
        let query = |after| CommandHistoryQuery {
            station: station.clone(),
            after,
            limit: PageLimit::new(1).unwrap(),
        };
        let cursor = block_on(store.read_command_history(query(None), scope.clone()))
            .expect("first page")
            .next_cursor
            .expect("cursor");
        drop(store);

        let connection = rusqlite::Connection::open(&database.0).expect("open database");
        assert_eq!(connection.execute(change, []).expect("modify anchor"), 1);
        drop(connection);

        let reopened = Store::open(&database.0, 8).expect("reopen store");
        assert_eq!(
            block_on(reopened.read_command_history(query(Some(cursor)), scope))
                .unwrap_err()
                .code(),
            StorageErrorCode::CursorExpired
        );
    }
}

fn command(id: &str, station: &str, connector_id: Option<u32>, hour: i64) -> Command<String> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: RequestId::new(id).unwrap(),
            correlation_id: None,
            resource: resource(station, connector_id),
            operation: CommandOperation::Start {
                authorization_reference: Some("private-auth-reference".to_owned()),
            },
            expires_at: timestamp(200),
        },
        AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("operator-private").unwrap(),
        },
    )
    .admit(timestamp(hour))
}
fn connector(id: u32) -> CanonicalResource {
    CanonicalResource::Connector {
        connector_id: CanonicalConnectorId::new(id.to_string()).unwrap(),
    }
}
fn resource(station: &str, connector_id: Option<u32>) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-test").unwrap(),
        station_id: StationId::new(station).unwrap(),
        resource: connector_id.map(connector),
        native_protocol_reference: None,
    }
}
fn result(command: &Command<String>, hour: i64) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: None,
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle: CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
        recorded_at: timestamp(hour),
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
    }
}
fn timestamp(hour: i64) -> UtcTimestamp {
    let base = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::September, 1).unwrap(),
        Time::MIDNIGHT,
    )
    .assume_offset(UtcOffset::UTC);
    UtcTimestamp::new(base + Duration::hours(hour))
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
