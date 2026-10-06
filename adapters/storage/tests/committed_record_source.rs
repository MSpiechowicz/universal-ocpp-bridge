use std::{
    future::Future,
    path::PathBuf,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use rusqlite::Connection;
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, CommittedRecord, CommittedRecordChunkQuery, CommittedRecordChunkResult,
    CommittedRecordCursor, CommittedRecordDescriptor, CommittedRecordField, CommittedRecordId,
    CommittedRecordQuery, Durability, EXPORT_RECORD_CHUNK_BYTES, OperationalStore, PageLimit,
    RuntimeResourceBudget, RuntimeResourceLimits, StorageErrorCode, StorageWritePurpose,
};
use uob_contracts::UtcTimestamp;
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-source-{}.sqlite3", Uuid::new_v4())))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}
fn block_on<F: Future>(future: F) -> F::Output {
    struct WakeThread(std::thread::Thread);
    impl Wake for WakeThread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(WakeThread(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => return result,
            Poll::Pending => std::thread::park(),
        }
    }
}
fn instant() -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        )
        .assume_offset(UtcOffset::UTC),
    )
}
fn record(id: &str, durability: Durability, payload: String) -> CommittedRecord<String> {
    CommittedRecord {
        record_id: CommittedRecordId::new(id.to_owned()).unwrap(),
        durability,
        committed_at: instant(),
        record: payload,
    }
}
fn commit(
    store: &Store,
    records: Vec<CommittedRecord<String>>,
) -> Result<(), uob_application::StorageError> {
    block_on(store.write_atomic(AtomicStoreWrite {
        charging_profile_201: None,
        reservation_16: None,
        reservation_observations_16: Vec::new(),
        reservation_201: None,
        reservation_observations_201: Vec::new(),
        firmware_16: None,
        firmware_observations_16: Vec::new(),
        purpose: StorageWritePurpose::Routine,
        station_snapshot: None,
        authorization_changes: vec![],
        command: None,
        command_result: None,
        journal_events: vec![],
        required_deliveries: vec![],
        committed_records: records,
    }))
    .map(|_| ())
}
fn query(
    durability: Durability,
    after: Option<CommittedRecordCursor>,
    limit: u16,
) -> CommittedRecordQuery {
    CommittedRecordQuery {
        durability,
        after,
        limit: PageLimit::new(limit).unwrap(),
    }
}
fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap()
}

fn payload(store: &Store, descriptor: &CommittedRecordDescriptor) -> String {
    let chunk = block_on(store.read_committed_record_chunk(
        CommittedRecordChunkQuery {
            token: descriptor.token.clone(),
            field: CommittedRecordField::Payload,
            offset: 0,
            max_bytes: EXPORT_RECORD_CHUNK_BYTES,
        },
        &budget(),
    ))
    .unwrap();
    let CommittedRecordChunkResult::Data(chunk) = chunk else {
        panic!("retained record expired")
    };
    serde_json::from_slice(&chunk.bytes).unwrap()
}

#[test]
fn empty_and_live_tail_resume_across_restart_with_independent_streams() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    let empty =
        block_on(store.read_committed_records(query(Durability::Critical, None, 2), &budget()))
            .unwrap();
    assert_eq!(
        (empty.high_water, empty.expired_prefix, empty.lost_records),
        (0, 0, 0)
    );
    assert!(!empty.legacy_baseline_incomplete);
    assert!(!empty.has_more);
    let start = empty.resume_cursor;
    commit(
        &store,
        vec![
            record("critical-1", Durability::Critical, "one".into()),
            record("telemetry-1", Durability::BestEffortTelemetry, "two".into()),
        ],
    )
    .unwrap();
    let page = block_on(store.read_committed_records(
        query(Durability::Critical, Some(start.clone()), 1),
        &budget(),
    ))
    .unwrap();
    assert_eq!(page.items[0].sequence, 1);
    assert_eq!(payload(&store, &page.items[0]), "one");
    assert_eq!(page.high_water, 1);
    let end = page.resume_cursor.clone();
    assert!(
        block_on(
            store.read_committed_records(
                query(Durability::Critical, Some(end.clone()), 1),
                &budget()
            )
        )
        .unwrap()
        .items
        .is_empty()
    );
    assert_eq!(
        block_on(store.read_committed_records(
            query(Durability::BestEffortTelemetry, Some(end), 1),
            &budget()
        ))
        .unwrap_err()
        .code(),
        StorageErrorCode::CursorExpired
    );
    assert_eq!(
        block_on(
            store
                .read_committed_records(query(Durability::BestEffortTelemetry, None, 1), &budget())
        )
        .unwrap()
        .items[0]
            .sequence,
        1
    );
    drop(store);
    let reopened = Store::open(&db.0, 8).unwrap();
    commit(
        &reopened,
        vec![record("critical-2", Durability::Critical, "three".into())],
    )
    .unwrap();
    let resumed = block_on(reopened.read_committed_records(
        query(Durability::Critical, Some(page.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert_eq!(resumed.items[0].sequence, 2);
    assert_eq!(payload(&reopened, &resumed.items[0]), "three");
}

#[test]
fn page_boundaries_and_large_record_metadata_never_drop_an_item() {
    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    commit(
        &store,
        (0..102)
            .map(|n| record(&format!("r-{n}"), Durability::Critical, n.to_string()))
            .collect(),
    )
    .unwrap();
    let first =
        block_on(store.read_committed_records(query(Durability::Critical, None, 100), &budget()))
            .unwrap();
    assert_eq!(first.items.len(), 100);
    assert!(first.has_more);
    assert_eq!(first.high_water, 102);
    let second = block_on(store.read_committed_records(
        query(Durability::Critical, Some(first.resume_cursor), 100),
        &budget(),
    ))
    .unwrap();
    assert_eq!(
        second.items.iter().map(|v| v.sequence).collect::<Vec<_>>(),
        vec![101, 102]
    );
    assert!(!second.has_more);

    let db = Database::new();
    let store = Store::open(&db.0, 8).unwrap();
    commit(
        &store,
        vec![
            record("big-1", Durability::Critical, "a".repeat(600_000)),
            record("big-2", Durability::Critical, "b".repeat(600_000)),
        ],
    )
    .unwrap();
    let first =
        block_on(store.read_committed_records(query(Durability::Critical, None, 100), &budget()))
            .unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(!first.has_more);
    let next = block_on(store.read_committed_records(
        query(Durability::Critical, Some(first.resume_cursor), 100),
        &budget(),
    ))
    .unwrap();
    assert_eq!(next.items.len(), 0);
    assert!(!next.has_more);
    commit(
        &store,
        vec![record(
            "too-big",
            Durability::Critical,
            "x".repeat(1_048_576),
        )],
    )
    .unwrap();
    let jumbo = block_on(store.read_committed_records(
        query(Durability::Critical, Some(next.resume_cursor), 100),
        &budget(),
    ))
    .unwrap();
    assert_eq!(jumbo.items[0].sequence, 3);
    assert!(jumbo.items[0].payload_len > 1_048_576);
    assert_eq!(jumbo.high_water, 3);
}

#[path = "committed_record_source/duplicate_cursor.rs"]
mod duplicate_cursor;
#[path = "committed_record_source/retention.rs"]
mod retention;
#[path = "committed_record_source/source_api.rs"]
mod source_api;

#[test]
fn version_eleven_upgrade_preserves_payload_and_marks_unknown_history() {
    let db = Database::new();
    let connection = Connection::open(&db.0).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE committed_records (
        row_id INTEGER PRIMARY KEY AUTOINCREMENT, record_id TEXT NOT NULL UNIQUE,
        durability INTEGER NOT NULL, committed_at TEXT NOT NULL, payload TEXT NOT NULL,
        retain_until INTEGER);
        INSERT INTO committed_records(record_id, durability, committed_at, payload, retain_until)
            VALUES ('legacy', 0, '\"2026-09-01T12:00:00Z\"', '\"preserved\"', 2000000000);
        PRAGMA user_version = 11;",
        )
        .unwrap();
    drop(connection);
    let store = Store::open(&db.0, 8).unwrap();
    let page =
        block_on(store.read_committed_records(query(Durability::Critical, None, 1), &budget()))
            .unwrap();
    assert!(page.legacy_baseline_incomplete);
    assert_eq!(payload(&store, &page.items[0]), "preserved");
    assert_eq!(page.items[0].sequence, 1);
    assert_eq!(page.expired_prefix, 0);
    drop(store);
    let reopened = Store::open(&db.0, 8).unwrap();
    let resumed = block_on(reopened.read_committed_records(
        query(Durability::Critical, Some(page.resume_cursor), 1),
        &budget(),
    ))
    .unwrap();
    assert!(resumed.legacy_baseline_incomplete);
    assert!(resumed.items.is_empty());
}
