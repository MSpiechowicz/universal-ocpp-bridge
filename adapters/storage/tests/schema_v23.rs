//! Operational SQLite schema v23: security-event dedup index and downgrade fence.
use std::path::PathBuf;

use rusqlite::Connection;
use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::{
    ArtifactDigest, BridgeId, ContractVersion, Environment, EventEnvelope, EventId, EventOrigin,
    EventType, ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity, SecurityEvent16,
    StationEvent, StationId, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, StationEvent, (), ()>;

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-schema-v23-{}.db", Uuid::new_v4())))
    }
    fn connection(&self) -> Connection {
        Connection::open(&self.0).unwrap()
    }
    fn version(&self) -> i64 {
        self.connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }
    fn has_index(&self) -> bool {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'security16_station_events'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

fn timestamp() -> UtcTimestamp {
    serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap()
}

fn security_event() -> EventEnvelope<StationEvent> {
    EventEnvelope {
        event_id: EventId::new("security-one").unwrap(),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("test").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new("process").unwrap(),
        },
        resource: ResourceRef {
            bridge_id: BridgeId::new("bridge").unwrap(),
            station_id: StationId::new("charger").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        source_time: Some(timestamp()),
        observed_at: timestamp(),
        event_type: EventType::new("station.security_event.16").unwrap(),
        origin: EventOrigin::Station,
        sequence: 1,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::SecurityEvent16 {
            station_snapshot_invalidated: StationId::new("charger").unwrap(),
            security_event_16: SecurityEvent16 {
                event_type: "StartupOfTheDevice".to_owned(),
                standard: true,
                timestamp: timestamp(),
                tech_info: None,
                tech_info_redactions: 0,
                shed_calls: 0,
            },
        },
    }
}

#[tokio::test]
async fn v22_database_gains_the_security_index_and_reports_v23() {
    let database = Database::new();
    drop(Store::open(&database.0, 16).unwrap());
    database
        .connection()
        .execute_batch("DROP INDEX security16_station_events; PRAGMA user_version = 22;")
        .unwrap();
    assert!(!database.has_index());

    let store = Store::open(&database.0, 16).unwrap();
    let mut write = AtomicStoreWrite::<String, StationEvent, (), ()>::empty();
    write.journal_events.push(security_event());
    store.write_atomic(write).await.unwrap();
    drop(store);

    assert!(database.has_index());
    assert_eq!(database.version(), 23);
    let plan: Vec<String> = {
        let connection = database.connection();
        let mut statement = connection
            .prepare(
                "EXPLAIN QUERY PLAN SELECT 1 FROM journal_events
                 WHERE resource = ?1
                   AND json_extract(payload,'$.payload.security_event_16.timestamp') = ?2
                   AND json_extract(payload,'$.payload.security_event_16.event_type') = ?3
                   AND json_extract(payload,'$.payload.security_event_16.tech_info') IS ?4
                   AND json_valid(payload)
                   AND json_type(payload,'$.payload.security_event_16') IS NOT NULL",
            )
            .unwrap();
        statement
            .query_map(["unused", "2026-09-01T00:00:00Z", "t", "x"], |row| {
                row.get(3)
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let plan = plan.join("\n");
    assert!(
        plan.contains("SEARCH journal_events USING INDEX security16_station_events"),
        "{plan}"
    );
    assert_eq!(plan.matches("=?").count(), 4, "{plan}");

    let found: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE json_extract(payload,'$.payload.security_event_16.timestamp') = ?1
               AND json_type(payload,'$.payload.security_event_16') IS NOT NULL",
            [serde_json::to_value(timestamp()).unwrap().as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}

#[test]
fn v24_database_is_refused_and_left_untouched() {
    let database = Database::new();
    drop(Store::open(&database.0, 16).unwrap());
    database
        .connection()
        .execute_batch("PRAGMA user_version = 24;")
        .unwrap();

    assert!(Store::open(&database.0, 16).is_err());
    assert_eq!(database.version(), 24);
}
