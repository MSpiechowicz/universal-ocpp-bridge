use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use rusqlite::Connection;
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AcknowledgementScope, AtomicStoreWrite, DeliveryAttempt, DeliveryAttemptResolution, DeliveryId,
    DeliveryOutcome, DeliveryReport, Durability, OperationalStore, PageLimit, PendingDelivery,
    PendingDeliveryQuery, RecoveryQuery, RetainedEventQuery, StorageErrorCode, TargetDeliveryStore,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, ContractVersion, Environment, EventEnvelope, EventId, EventOrigin,
    EventType, ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity, StationId,
    TargetInstanceId, UtcTimestamp,
};
use uob_storage_adapter::{MINIMUM_SQLITE_VERSION, SqliteOperationalStore};
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;
type DeliveryStore = SqliteOperationalStore<String, String, String, String>;

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
fn upgrades_existing_outbox_schema_without_losing_pending_rows() {
    let database = TestDatabase::new();
    let connection = Connection::open(database.path()).expect("open legacy database");
    connection
        .execute_batch(
            "CREATE TABLE target_deliveries (\n\
                 target_instance_id TEXT NOT NULL, target_revision INTEGER NOT NULL,\n\
                 event_id TEXT NOT NULL, delivery_id TEXT NOT NULL, ordering_key TEXT NOT NULL,\n\
                 deadline TEXT NOT NULL, durability INTEGER NOT NULL, payload TEXT NOT NULL,\n\
                 PRIMARY KEY(target_instance_id, target_revision, event_id),\n\
                 UNIQUE(delivery_id)\n\
             );\n\
             PRAGMA user_version = 1;",
        )
        .expect("create version-one schema");
    drop(connection);

    let store = DeliveryStore::open(database.path(), 8).expect("migrate legacy store");
    assert!(ready_deliveries(&store, timestamp(1)).is_empty());
    drop(store);
    let reopened = Connection::open(database.path()).expect("inspect migrated database");
    let version: i64 = reopened
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, 2);
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

#[test]
fn retry_attempts_survive_reopen_and_preserve_exact_outcomes() {
    let database = TestDatabase::new();
    let store = DeliveryStore::open(database.path(), 8).expect("open SQLite store");
    block_on(store.write_atomic(delivery_write("event-retry", "delivery-retry")))
        .expect("commit delivery");

    let exposed = DeliveryAttempt {
        report: DeliveryReport {
            delivery_id: text(DeliveryId::new, "delivery-retry"),
            outcome: DeliveryOutcome::LocallyExposed {
                surface: "ems.http.sse".to_owned(),
            },
            reported_at: timestamp(1),
        },
        resolution: DeliveryAttemptResolution::RetryAt(timestamp(2)),
    };
    block_on(store.record_delivery_attempt(exposed)).expect("record retry");
    assert!(ready_deliveries(&store, timestamp(1)).is_empty());
    drop(store);

    let reopened = DeliveryStore::open(database.path(), 8).expect("reopen SQLite store");
    let ready = ready_deliveries(&reopened, timestamp(2));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].attempt_count, 1);
    assert_eq!(ready[0].delivery.delivery_id.as_str(), "delivery-retry");
    block_on(reopened.record_delivery_attempt(DeliveryAttempt {
        report: DeliveryReport {
            delivery_id: text(DeliveryId::new, "delivery-retry"),
            outcome: DeliveryOutcome::Acknowledged {
                peer: "ems-client-a".to_owned(),
                scope: AcknowledgementScope("application-processing".to_owned()),
            },
            reported_at: timestamp(3),
        },
        resolution: DeliveryAttemptResolution::Final,
    }))
    .expect("record acknowledgement");

    assert!(ready_deliveries(&reopened, timestamp(4)).is_empty());
    let attempts = block_on(reopened.delivery_attempts(
        text(DeliveryId::new, "delivery-retry"),
        PageLimit::new(10).expect("limit"),
    ))
    .expect("attempt history");
    assert_eq!(attempts.len(), 2);
    assert!(matches!(
        attempts[0].outcome,
        DeliveryOutcome::Acknowledged { ref peer, .. } if peer == "ems-client-a"
    ));
    assert!(matches!(
        attempts[1].outcome,
        DeliveryOutcome::LocallyExposed { ref surface } if surface == "ems.http.sse"
    ));
}

#[test]
fn pending_reads_preserve_resource_order_without_blocking_other_resources() {
    let database = TestDatabase::new();
    let store = DeliveryStore::open(database.path(), 8).expect("open SQLite store");
    let mut write = delivery_write("event-first", "delivery-first");
    write
        .journal_events
        .push(event_for("event-second", resource(), 2));
    write.required_deliveries.push(message_delivery(
        "event-second",
        "delivery-second",
        resource(),
    ));
    let other = ResourceRef {
        station_id: text(StationId::new, "station-other"),
        ..resource()
    };
    write
        .journal_events
        .push(event_for("event-other", other.clone(), 1));
    write
        .required_deliveries
        .push(message_delivery("event-other", "delivery-other", other));
    block_on(store.write_atomic(write)).expect("commit ordered deliveries");

    let ids = ready_deliveries(&store, timestamp(1))
        .into_iter()
        .map(|value| value.delivery.delivery_id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["delivery-first", "delivery-other"]);
}

fn ready_deliveries(
    store: &DeliveryStore,
    ready_at: UtcTimestamp,
) -> Vec<uob_application::ScheduledDelivery<String>> {
    block_on(store.read_pending_deliveries(PendingDeliveryQuery {
        target_instance_id: text(TargetInstanceId::new, "target-main"),
        target_configuration_revision: 4,
        ready_at,
        limit: PageLimit::new(10).expect("limit"),
    }))
    .expect("read ready deliveries")
}

fn delivery_write(
    event_id: &str,
    delivery_id: &str,
) -> AtomicStoreWrite<String, String, String, String> {
    AtomicStoreWrite {
        station_snapshot: None,
        authorization_changes: Vec::new(),
        command: None,
        command_result: None,
        journal_events: vec![event(event_id)],
        required_deliveries: vec![message_delivery(event_id, delivery_id, resource())],
        committed_records: Vec::new(),
    }
}

fn message_delivery(
    event_id: &str,
    delivery_id: &str,
    ordering_key: ResourceRef,
) -> PendingDelivery<String> {
    PendingDelivery {
        delivery_id: text(DeliveryId::new, delivery_id),
        event_id: text(EventId::new, event_id),
        target_instance_id: text(TargetInstanceId::new, "target-main"),
        target_configuration_revision: 4,
        ordering_key,
        deadline: timestamp(8),
        durability: Durability::Critical,
        payload: format!("canonical target message for {event_id}"),
    }
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
    event_for(event_id, resource(), 1)
}

fn event_for(event_id: &str, resource: ResourceRef, sequence: u64) -> EventEnvelope<String> {
    EventEnvelope {
        event_id: text(EventId::new, event_id),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: text(ReleaseId::new, "release-test"),
            release_digest: text(ArtifactDigest::new, "sha256:test"),
            process_instance_id: text(ProcessInstanceId::new, "process-test"),
        },
        resource,
        source_time: None,
        observed_at: timestamp(0),
        event_type: text(EventType::new, "test.event.v1"),
        origin: EventOrigin::Bridge,
        sequence,
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
