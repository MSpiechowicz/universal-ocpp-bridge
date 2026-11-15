//! SQLite answers the security-event dedup read from the journal row a real
//! `record_security_event_16` commit wrote, scoped to one station and to retained rows.
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use uob_application::{
    AtomicStoreWrite, OperationalStore, SecurityEventKey16, SecurityEventObservation16,
    SecurityEventStore16, StationEvent, record_security_event_16,
    transaction16::TransactionContext,
};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, StationEvent, TransactionSnapshot, String>;

struct Database(PathBuf);

impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-security16-{}.db", Uuid::new_v4())))
    }

    fn open(&self) -> Store {
        Store::open(&self.0, 16).unwrap()
    }

    fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.0).unwrap()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(
        time::OffsetDateTime::from_unix_timestamp(1_791_306_000 + seconds).expect("instant"),
    )
}

fn parsed(text: &str) -> UtcTimestamp {
    serde_json::from_value(json!(text)).unwrap()
}

fn station(station_id: &str) -> StationSnapshot {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    snapshot.station.station_id = StationId::new(station_id).unwrap();
    snapshot.current_values.push(DataPointValue {
        point_id: PointId::new("ocpp16/registration/status").unwrap(),
        value: Some(TypedValue::Text("Accepted".to_owned())),
        source_time: None,
        observed_at: at(0),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    });
    snapshot
}

fn context(event_id: &str, sequence: u64) -> TransactionContext {
    TransactionContext {
        identity: serde_json::from_value(json!({
            "bridge_id":"bridge-berlin-1",
            "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test",
                "process_instance_id":"test"}
        }))
        .unwrap(),
        event_id: EventId::new(event_id).unwrap(),
        sequence,
        correlation_id: None,
        target: None,
        delivery_deadline: at(86_400),
    }
}

async fn record(
    store: &Store,
    snapshot: &mut StationSnapshot,
    observation: SecurityEventObservation16,
    sequence: u64,
) {
    record_security_event_16(
        store,
        snapshot,
        observation,
        0,
        context(&format!("security-{sequence}"), sequence),
        at(10),
    )
    .await
    .unwrap();
}

async fn recorded(store: &Store, key: SecurityEventKey16) -> bool {
    store.security_event_recorded_16(key).await.unwrap()
}

fn key(
    station: &StationSnapshot,
    event_type: &str,
    timestamp: UtcTimestamp,
    tech_info: Option<&str>,
) -> SecurityEventKey16 {
    SecurityEventKey16 {
        station: station.station.clone(),
        event_type: event_type.to_owned(),
        timestamp,
        tech_info: tech_info.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_committed_event_is_recognised_exactly_and_across_reopen() {
    let database = Database::new();
    let store = database.open();
    let mut charger = station("charger-one");
    let observation =
        SecurityEventObservation16::new("InvalidMessages", at(2), Some("bad \"frame\" \u{e9}"))
            .unwrap();
    let identity = observation.key(charger.station.clone());

    assert!(!recorded(&store, identity.clone()).await);
    record(&store, &mut charger, observation, 1).await;

    assert!(recorded(&store, identity.clone()).await);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert!(recorded(&reopened, identity).await);

    let same = |event_type, timestamp, tech_info| key(&charger, event_type, timestamp, tech_info);
    let tech = Some("bad \"frame\" \u{e9}");
    assert!(recorded(&reopened, same("InvalidMessages", at(2), tech)).await);
    assert!(!recorded(&reopened, same("InvalidMessages", at(2), Some("bad frame"))).await);
    assert!(!recorded(&reopened, same("InvalidMessages", at(2), None)).await);
    assert!(!recorded(&reopened, same("MemoryExhaustion", at(2), tech)).await);
    assert!(!recorded(&reopened, same("InvalidMessages", at(3), tech)).await);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn absent_tech_info_differs_from_any_present_tech_info() {
    let database = Database::new();
    let store = database.open();
    let mut charger = station("charger-one");
    let bare = SecurityEventObservation16::new("StartupOfTheDevice", at(5), None).unwrap();
    record(&store, &mut charger, bare, 1).await;

    assert!(recorded(&store, key(&charger, "StartupOfTheDevice", at(5), None)).await);
    assert!(!recorded(&store, key(&charger, "StartupOfTheDevice", at(5), Some(""))).await);
    assert!(
        !recorded(
            &store,
            key(&charger, "StartupOfTheDevice", at(5), Some("x"))
        )
        .await
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn one_station_never_suppresses_another() {
    let database = Database::new();
    let store = database.open();
    let mut first = station("charger-one");
    let second = station("charger-two");
    let observation = SecurityEventObservation16::new("InvalidMessages", at(2), Some("x")).unwrap();
    let other = observation.key(second.station.clone());
    record(&store, &mut first, observation, 1).await;

    assert!(!recorded(&store, other).await);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn instants_written_with_different_offsets_are_the_same_event() {
    let database = Database::new();
    let store = database.open();
    let mut charger = station("charger-one");
    let offset = parsed("2026-10-01T03:00:00+02:00");
    let utc = parsed("2026-10-01T01:00:00Z");
    let observation = SecurityEventObservation16::new("SettingSystemTime", offset, None).unwrap();
    record(&store, &mut charger, observation, 1).await;

    assert!(recorded(&store, key(&charger, "SettingSystemTime", utc, None)).await);
    assert!(recorded(&store, key(&charger, "SettingSystemTime", offset, None)).await);
    let stored: String = database
        .connection()
        .query_row(
            "SELECT json_extract(payload,'$.payload.security_event_16.timestamp') FROM journal_events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, "2026-10-01T01:00:00Z");
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_pruned_event_is_no_longer_recorded() {
    let database = Database::new();
    let store = database.open();
    let mut charger = station("charger-one");
    let observation = SecurityEventObservation16::new("InvalidMessages", at(2), None).unwrap();
    let identity = observation.key(charger.station.clone());
    record(&store, &mut charger, observation, 1).await;
    assert!(recorded(&store, identity.clone()).await);

    store.maintain_storage_retention(at(0)).await.unwrap();
    assert!(recorded(&store, identity.clone()).await);

    let far_future = UtcTimestamp::new(
        time::OffsetDateTime::from_unix_timestamp(1_791_306_000 + 10 * 365 * 86_400).unwrap(),
    );
    store.maintain_storage_retention(far_future).await.unwrap();
    assert!(!recorded(&store, identity).await);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn unrelated_malformed_journal_rows_do_not_break_the_read() {
    let database = Database::new();
    let store = database.open();
    let mut charger = station("charger-one");
    let observation = SecurityEventObservation16::new("InvalidMessages", at(2), None).unwrap();
    let identity = observation.key(charger.station.clone());
    record(&store, &mut charger, observation, 1).await;

    let connection = database.connection();
    let resource: String = connection
        .query_row("SELECT resource FROM journal_events", [], |row| row.get(0))
        .unwrap();
    connection
        .execute(
            "INSERT INTO journal_events(event_id, resource, sequence, payload, retain_until)
             VALUES ('garbage', ?1, 999, '{not json', NULL)",
            [&resource],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO journal_events(event_id, resource, sequence, payload, retain_until)
             VALUES ('other-kind', ?1, 1000, '{\"payload\":{\"class\":\"Heartbeat\"}}', NULL)",
            [&resource],
        )
        .unwrap();
    drop(connection);

    assert!(recorded(&store, identity).await);
    assert!(!recorded(&store, key(&charger, "InvalidMessages", at(9), None)).await);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

/// Texts whose JSON encoding differs from their value: escapes, control characters (serialized
/// as `\u0001`), non-ASCII and a literal that looks like an escape.
const AWKWARD: [&str; 9] = [
    "",
    "null",
    "say \"hi\" \\ done",
    "caf\u{e9} \u{1f50c} \u{4e2d}\u{6587}",
    "bell \u{1} tab \t newline \n",
    "literal \\u0041 and A",
    "trailing space ",
    "nul \u{0} inside",
    "\u{0}",
];

#[tokio::test]
async fn escaped_and_special_tech_info_is_matched_exactly() {
    let database = Database::new();
    let store = database.open();
    let mut charger = station("charger-one");
    for (index, text) in AWKWARD.iter().enumerate() {
        let observation =
            SecurityEventObservation16::new("InvalidMessages", at(2), Some(text)).unwrap();
        record(&store, &mut charger, observation, index as u64 + 1).await;
    }

    for text in AWKWARD {
        let found = recorded(&store, key(&charger, "InvalidMessages", at(2), Some(text))).await;
        assert!(found, "{text:?} was stored but is not recognised");
    }
    for text in [
        "A",
        "literal A and A",
        "bell  tab",
        "nul  inside",
        "trailing space",
        "nul ",
    ] {
        let found = recorded(&store, key(&charger, "InvalidMessages", at(2), Some(text))).await;
        assert!(!found, "{text:?} was never stored");
    }
    assert!(!recorded(&store, key(&charger, "InvalidMessages", at(2), None)).await);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

/// The same envelope a real commit journals, one per `techInfo`.
fn journaled(number: u64, tech_info: Option<String>) -> EventEnvelope<StationEvent> {
    let station_id = StationId::new("charger-one").unwrap();
    let identity = context(&format!("flood-{number}"), number);

    EventEnvelope {
        event_id: identity.event_id,
        schema_version: ContractVersion::V1_INITIAL,
        runtime: identity.identity.runtime,
        resource: ResourceRef {
            bridge_id: identity.identity.bridge_id,
            station_id: station_id.clone(),
            resource: None,
            native_protocol_reference: None,
        },
        source_time: Some(at(0)),
        observed_at: at(0),
        event_type: EventType::new("station.security_event.16").unwrap(),
        origin: EventOrigin::Station,
        sequence: number,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::SecurityEvent16 {
            station_snapshot_invalidated: station_id,
            security_event_16: SecurityEvent16 {
                event_type: "InvalidMessages".to_owned(),
                standard: true,
                timestamp: at(0),
                tech_info,
                tech_info_redactions: 0,
                shed_calls: 0,
            },
        },
    }
}

#[tokio::test]
async fn an_embedded_nul_is_neither_truncated_nor_confused_with_other_text() {
    for (stored, other) in [
        ("\u{0}", ""),
        ("", "\u{0}"),
        ("a\u{0}b", "a"),
        ("a", "a\u{0}b"),
    ] {
        let database = Database::new();
        let store = database.open();
        let mut charger = station("charger-one");
        let observation =
            SecurityEventObservation16::new("InvalidMessages", at(2), Some(stored)).unwrap();
        record(&store, &mut charger, observation, 1).await;

        assert!(
            recorded(
                &store,
                key(&charger, "InvalidMessages", at(2), Some(stored))
            )
            .await
        );
        let confused = recorded(&store, key(&charger, "InvalidMessages", at(2), Some(other))).await;
        assert!(!confused, "{stored:?} was taken for {other:?}");
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}

#[tokio::test]
async fn a_flooded_timestamp_still_answers_present_absent_empty_and_special_text() {
    const FLOODED: u64 = 10_000;
    let database = Database::new();
    let store = database.open();
    let charger = station("charger-one");
    let started = std::time::Instant::now();
    let mut write = AtomicStoreWrite::<String, StationEvent, TransactionSnapshot, String>::empty();
    for number in 0..FLOODED {
        write
            .journal_events
            .push(journaled(number + 1, Some(format!("detail {number}"))));
    }
    write.journal_events.push(journaled(FLOODED + 1, None));
    write
        .journal_events
        .push(journaled(FLOODED + 2, Some("q\"\u{1}\u{e9}".to_owned())));
    store.write_atomic(write).await.unwrap();
    let stored = started.elapsed();

    let started = std::time::Instant::now();
    let probes = [
        (Some("detail 0"), true),
        (Some("detail 9999"), true),
        (Some("detail 10000"), false),
        (Some(""), false),
        (None, true),
        (Some("q\"\u{1}\u{e9}"), true),
        (Some("q\"\u{2}\u{e9}"), false),
    ];
    for (tech_info, expected) in probes {
        let found = recorded(&store, key(&charger, "InvalidMessages", at(0), tech_info)).await;
        assert_eq!(found, expected, "{tech_info:?}");
    }
    let looked_up = started.elapsed();
    store.shutdown(Duration::from_secs(1)).await.unwrap();

    // Structural boundedness is proven by the unit tests (index search, VM steps); this only
    // guards against the lookup scaling with the flood: decoding 10,000 rows per probe would
    // cost far more than the single batched insert did.
    assert!(
        looked_up < Duration::from_secs(5),
        "7 lookups took {looked_up:?} after {FLOODED} inserts took {stored:?}"
    );
}
