use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, DeliveryId, Durability, OperationalStore, PageLimit, PendingDelivery,
    RecoveryQuery, RetainedEventQuery, StorageErrorCode,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, ContractVersion, Environment, EventEnvelope, EventId, EventOrigin,
    EventType, ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity, StationId,
    TargetInstanceId, UtcTimestamp,
};
use uob_storage_adapter::{MINIMUM_SQLITE_VERSION, SqliteOperationalStore};
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-storage-{}.sqlite3", Uuid::new_v4())))
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
fn reports_required_runtime_configuration() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open configured SQLite store");
    let configuration = store.runtime_configuration();

    assert_eq!(configuration.journal_mode, "wal");
    assert_eq!(configuration.synchronous, 2);
    assert!(version_at_least(
        &configuration.sqlite_version,
        MINIMUM_SQLITE_VERSION
    ));
}

#[test]
fn failed_delivery_insert_rolls_back_event_after_reopen() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    block_on(store.write_atomic(write_with(
        Vec::new(),
        vec![delivery("event-atomic", 4, "delivery-existing")],
    )))
    .expect("seed conflicting delivery key");

    let error = block_on(store.write_atomic(write_with(
        vec![event("event-atomic")],
        vec![delivery("event-atomic", 4, "delivery-conflict")],
    )))
    .expect_err("delivery conflict aborts transaction");
    assert_eq!(error.code(), StorageErrorCode::Conflict);
    drop(store);

    let reopened = Store::open(database.path(), 8).expect("reopen SQLite store");
    let events = block_on(reopened.read_retained_events(RetainedEventQuery {
        resource: resource(),
        after: None,
        limit: PageLimit::new(10).expect("page limit"),
    }))
    .expect("read journal after reopen");
    assert!(
        events.items.is_empty(),
        "journal insert must have rolled back"
    );

    let recovery = block_on(reopened.recover(RecoveryQuery {
        limit: PageLimit::new(10).expect("recovery limit"),
    }))
    .expect("recover deliveries after reopen");
    assert_eq!(recovery.pending_deliveries.len(), 1);
    assert_eq!(
        recovery.pending_deliveries[0].delivery_id.as_str(),
        "delivery-existing"
    );
}

#[test]
fn same_event_is_independent_across_target_revisions() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    block_on(store.write_atomic(write_with(
        vec![event("event-revision")],
        vec![
            delivery("event-revision", 4, "delivery-r4"),
            delivery("event-revision", 5, "delivery-r5"),
        ],
    )))
    .expect("commit two target revisions");
    drop(store);

    let reopened = Store::open(database.path(), 8).expect("reopen SQLite store");
    let recovery = block_on(reopened.recover(RecoveryQuery {
        limit: PageLimit::new(10).expect("recovery limit"),
    }))
    .expect("recover target revisions");
    let revisions = recovery
        .pending_deliveries
        .iter()
        .map(|delivery| delivery.target_configuration_revision)
        .collect::<Vec<_>>();
    assert_eq!(revisions, vec![4, 5]);
    assert!(
        recovery
            .pending_deliveries
            .iter()
            .all(|delivery| delivery.event_id.as_str() == "event-revision")
    );
}

fn write_with(
    events: Vec<EventEnvelope<String>>,
    deliveries: Vec<PendingDelivery<String>>,
) -> AtomicStoreWrite<String, String, String, String> {
    AtomicStoreWrite {
        station_snapshot: None,
        authorization_changes: Vec::new(),
        command: None,
        command_result: None,
        journal_events: events,
        required_deliveries: deliveries,
        committed_records: Vec::new(),
    }
}

fn delivery(event_id: &str, revision: u64, delivery_id: &str) -> PendingDelivery<String> {
    PendingDelivery {
        delivery_id: text(DeliveryId::new, delivery_id),
        event_id: text(EventId::new, event_id),
        target_instance_id: text(TargetInstanceId::new, "target-main"),
        target_configuration_revision: revision,
        ordering_key: resource(),
        deadline: timestamp(8),
        durability: Durability::Critical,
        payload: "canonical event delivery".to_owned(),
    }
}

fn event(event_id: &str) -> EventEnvelope<String> {
    EventEnvelope {
        event_id: text(EventId::new, event_id),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: text(ReleaseId::new, "release-test"),
            release_digest: text(ArtifactDigest::new, "sha256:test"),
            process_instance_id: text(ProcessInstanceId::new, "process-test"),
        },
        resource: resource(),
        source_time: None,
        observed_at: timestamp(0),
        event_type: text(EventType::new, "test.event.v1"),
        origin: EventOrigin::Bridge,
        sequence: 1,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: "test event".to_owned(),
    }
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-test"),
        station_id: text(StationId::new, "station-test"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp(minute: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).expect("test date"),
            Time::from_hms(12, minute, 0).expect("test time"),
        )
        .assume_offset(UtcOffset::UTC),
    )
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

fn version_at_least(actual: &str, minimum: &str) -> bool {
    let components = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u64>().expect("numeric SQLite version"))
            .collect::<Vec<_>>()
    };
    components(actual) >= components(minimum)
}
