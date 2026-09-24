use rusqlite::{Connection, params};
use uob_application::{
    OperationalStore, PageLimit, RetainedEventQuery, SnapshotQuery, StationEvent, StorageErrorCode,
};
use uob_contracts::{
    EventEnvelope, EventId, EventOrigin, EventType, ResourceRef, StationSnapshot,
    TransactionSnapshot,
};
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<(), StationEvent, TransactionSnapshot, ()>;

fn snapshot() -> StationSnapshot {
    serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap()
}

fn legacy_file() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("uob-legacy-station-{}.db", uuid::Uuid::new_v4()))
}

fn seed(connection: &Connection, snapshot: &StationSnapshot) {
    connection
        .execute(
            "INSERT INTO station_snapshots(station_key, payload) VALUES (?1, ?2)",
            params![
                serde_json::to_string(&snapshot.station).unwrap(),
                serde_json::to_string(snapshot).unwrap()
            ],
        )
        .unwrap();
}

#[tokio::test]
async fn legacy_controller_address_keeps_payload_and_resolves_canonical_scoped_key() {
    let file = legacy_file();
    let snapshot = snapshot();
    let connection = Connection::open(&file).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE station_snapshots(station_key TEXT PRIMARY KEY, payload TEXT NOT NULL);
         CREATE TABLE journal_events(row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL UNIQUE, resource TEXT NOT NULL, sequence INTEGER NOT NULL,
            payload TEXT NOT NULL, retain_until INTEGER, UNIQUE(resource, sequence));
         PRAGMA user_version = 7;",
        )
        .unwrap();
    seed(&connection, &snapshot);
    let event = EventEnvelope {
        event_id: EventId::new("legacy-status-1").unwrap(),
        schema_version: snapshot.schema_version,
        runtime: serde_json::from_value(serde_json::json!({
            "environment":"demo", "release_id":"test", "release_digest":"sha256:test",
            "process_instance_id":"test-process"
        }))
        .unwrap(),
        resource: snapshot.station.clone(),
        source_time: None,
        observed_at: snapshot.observed_at,
        event_type: EventType::new("station.availability.observed").unwrap(),
        origin: EventOrigin::Station,
        sequence: 1,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::StationSnapshot(snapshot.clone()),
    };
    connection.execute(
        "INSERT INTO journal_events(event_id, resource, sequence, payload) VALUES (?1, ?2, ?3, ?4)",
        params![event.event_id.as_str(), serde_json::to_string(&snapshot.station).unwrap(),
            i64::try_from(event.sequence).unwrap(), serde_json::to_string(&event).unwrap()],
    ).unwrap();
    drop(connection);
    let store = Store::open(&file, 8).unwrap();
    let station = ResourceRef {
        bridge_id: snapshot.station.bridge_id.clone(),
        station_id: snapshot.station.station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    };
    assert_eq!(
        store.station_snapshot(station.clone()).await.unwrap(),
        Some(snapshot.clone())
    );
    let page = store
        .read_scoped_snapshots(
            SnapshotQuery {
                after: None,
                limit: PageLimit::new(1).unwrap(),
            },
            vec![station.clone()],
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![snapshot.clone()]);
    let events = store
        .read_retained_events(RetainedEventQuery {
            resource: station.clone(),
            after: None,
            limit: PageLimit::new(1).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(events.events, vec![event]);
    let checkpoint = events.resume_cursor.unwrap();
    store
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    let reopened = Store::open(&file, 8).unwrap();
    let resumed = reopened
        .read_retained_events(RetainedEventQuery {
            resource: station,
            after: Some(checkpoint.clone()),
            limit: PageLimit::new(1).unwrap(),
        })
        .await
        .unwrap();
    assert!(resumed.events.is_empty());
    assert_eq!(resumed.resume_cursor, Some(checkpoint));
    reopened
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    drop(reopened);
    let _ = std::fs::remove_file(file);
}

#[test]
fn ambiguous_legacy_station_keys_fail_without_partial_migration() {
    let file = legacy_file();
    let snapshot = snapshot();
    let mut second = snapshot.clone();
    second.station.native_protocol_reference = None;
    let connection = Connection::open(&file).unwrap();
    connection.execute_batch("CREATE TABLE station_snapshots(station_key TEXT PRIMARY KEY, payload TEXT NOT NULL); PRAGMA user_version = 7;").unwrap();
    seed(&connection, &snapshot);
    seed(&connection, &second);
    drop(connection);
    let error = Store::open(&file, 8).err().unwrap();
    assert_eq!(error.code(), StorageErrorCode::Conflict);
    let connection = Connection::open(&file).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM station_snapshots", [], |row| {
            row.get(0)
        })
        .unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(version, 7);
    drop(connection);
    let _ = std::fs::remove_file(file);
}

fn legacy_journal_event(snapshot: &StationSnapshot, number: u64) -> EventEnvelope<StationEvent> {
    EventEnvelope {
        event_id: EventId::new(format!("legacy-{number}")).unwrap(),
        schema_version: snapshot.schema_version,
        runtime: serde_json::from_value(serde_json::json!({
            "environment":"demo", "release_id":"test", "release_digest":"sha256:test",
            "process_instance_id":"test-process"
        }))
        .unwrap(),
        resource: snapshot.station.clone(),
        source_time: None,
        observed_at: snapshot.observed_at,
        event_type: EventType::new("station.availability.observed").unwrap(),
        origin: EventOrigin::Station,
        sequence: number,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::StationSnapshot(snapshot.clone()),
    }
}

#[tokio::test]
async fn journal_upgrade_processes_many_rows_and_rolls_back_a_late_collision() {
    let file = legacy_file();
    let snapshot = snapshot();
    let mut different_key = snapshot.clone();
    different_key.station.native_protocol_reference = None;
    let connection = Connection::open(&file).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE journal_events(row_id INTEGER PRIMARY KEY AUTOINCREMENT,
         event_id TEXT NOT NULL UNIQUE, resource TEXT NOT NULL, sequence INTEGER NOT NULL,
         payload TEXT NOT NULL, retain_until INTEGER, UNIQUE(resource, sequence));
         PRAGMA user_version = 7;",
        )
        .unwrap();
    let legacy_key = serde_json::to_string(&snapshot.station).unwrap();
    let conflicting_key = serde_json::to_string(&different_key.station).unwrap();
    for number in 1..=260 {
        let mut event = legacy_journal_event(&snapshot, number);
        let key = if number == 260 {
            event.resource = different_key.station.clone();
            &conflicting_key
        } else {
            &legacy_key
        };
        // The final row collides with row 259 only after both legacy keys normalize.
        if number == 260 {
            event.sequence = 259;
        }
        connection.execute(
            "INSERT INTO journal_events(event_id, resource, sequence, payload) VALUES (?1, ?2, ?3, ?4)",
            params![event.event_id.as_str(), key, i64::try_from(event.sequence).unwrap(),
                serde_json::to_string(&event).unwrap()],
        ).unwrap();
    }
    drop(connection);
    assert_eq!(
        Store::open(&file, 8).err().unwrap().code(),
        StorageErrorCode::Conflict
    );
    let connection = Connection::open(&file).unwrap();
    let (count, legacy_count, version): (i64, i64, i64) = (
        connection
            .query_row("SELECT COUNT(*) FROM journal_events", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row(
                "SELECT COUNT(*) FROM journal_events WHERE resource = ?1",
                [&legacy_key],
                |row| row.get(0),
            )
            .unwrap(),
        connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap(),
    );
    assert_eq!((count, legacy_count, version), (260, 259, 7));
    connection
        .execute(
            "DELETE FROM journal_events WHERE event_id = 'legacy-260'",
            [],
        )
        .unwrap();
    drop(connection);
    let store = Store::open(&file, 8).unwrap();
    store
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    let connection = Connection::open(&file).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM journal_events WHERE resource = ?1",
                [&legacy_key],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM journal_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        259
    );
    drop(connection);
    let _ = std::fs::remove_file(file);
}

#[tokio::test]
async fn upgraded_database_does_not_redecode_retained_journal_on_reopen() {
    let file = legacy_file();
    let store = Store::open(&file, 8).unwrap();
    store
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    drop(store);
    let connection = Connection::open(&file).unwrap();
    connection.execute(
        "INSERT INTO journal_events(event_id, resource, sequence, payload) VALUES ('invalid-payload',
            'retained', 1, '{not json}')",
        [],
    ).unwrap();
    drop(connection);
    let reopened = Store::open(&file, 8).unwrap();
    reopened
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    drop(reopened);
    let _ = std::fs::remove_file(file);
}

#[test]
fn legacy_journal_corruption_rejects_the_entire_upgrade() {
    let file = legacy_file();
    let connection = Connection::open(&file).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE journal_events(row_id INTEGER PRIMARY KEY AUTOINCREMENT,
         event_id TEXT NOT NULL UNIQUE, resource TEXT NOT NULL, sequence INTEGER NOT NULL,
         payload TEXT NOT NULL, retain_until INTEGER, UNIQUE(resource, sequence));
         INSERT INTO journal_events(event_id, resource, sequence, payload)
         VALUES ('corrupt', 'not-a-resource', 1, '{not json}');
         PRAGMA user_version = 7;",
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        Store::open(&file, 8).err().unwrap().code(),
        StorageErrorCode::IntegrityFailure
    );
    let connection = Connection::open(&file).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        connection
            .query_row("SELECT payload FROM journal_events", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "{not json}"
    );
    drop(connection);
    let _ = std::fs::remove_file(file);
}
