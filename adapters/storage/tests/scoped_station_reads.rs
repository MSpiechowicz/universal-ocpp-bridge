use uob_application::{
    AtomicStoreWrite, OperationalStore, PageLimit, SnapshotQuery, StationEvent, StorageErrorCode,
};
use uob_contracts::{
    BridgeId, Connectivity, EventEnvelope, EventId, EventOrigin, EventType, ResourceRef, StationId,
    StationSnapshot, TransactionSnapshot,
};
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<(), StationEvent, TransactionSnapshot, ()>;

fn snapshot(bridge: &str, station: &str) -> StationSnapshot {
    let mut value: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    value.station.bridge_id = BridgeId::new(bridge).unwrap();
    value.station.station_id = StationId::new(station).unwrap();
    value.station.resource = None;
    value.station.native_protocol_reference = None;
    value
}

fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("uob-scoped-{}.db", uuid::Uuid::new_v4()))
}

async fn commit(store: &Store, snapshot: StationSnapshot) {
    let mut write = AtomicStoreWrite::<(), StationEvent, TransactionSnapshot, ()>::empty();
    write.station_snapshot = Some(snapshot);
    store.write_atomic(write).await.unwrap();
}

#[tokio::test]
async fn whitelist_precedes_limit_and_cursor_and_exact_key_is_bridge_qualified() {
    let file = path();
    let store = Store::open(&file, 16).unwrap();
    let visible_one = snapshot("bridge-a", "b");
    let visible_two = snapshot("bridge-a", "d");
    let same_station_other_bridge = snapshot("bridge-b", "b");
    for state in [
        snapshot("bridge-a", "a"),
        visible_one.clone(),
        snapshot("bridge-a", "c"),
        visible_two.clone(),
        same_station_other_bridge.clone(),
    ] {
        commit(&store, state).await;
    }
    let scope = vec![visible_one.station.clone(), visible_two.station.clone()];
    let first = store
        .read_scoped_snapshots(
            SnapshotQuery {
                after: None,
                limit: PageLimit::new(1).unwrap(),
            },
            scope.clone(),
        )
        .await
        .unwrap();
    assert_eq!(first.items, vec![visible_one.clone()]);
    let second = store
        .read_scoped_snapshots(
            SnapshotQuery {
                after: first.next_cursor,
                limit: PageLimit::new(1).unwrap(),
            },
            scope,
        )
        .await
        .unwrap();
    assert_eq!(second.items, vec![visible_two.clone()]);
    assert!(second.next_cursor.is_none());
    assert!(
        store
            .read_scoped_snapshots(
                SnapshotQuery {
                    after: None,
                    limit: PageLimit::new(1).unwrap(),
                },
                Vec::new()
            )
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .station_snapshot(visible_one.station.clone())
            .await
            .unwrap(),
        Some(visible_one.clone())
    );
    assert_eq!(
        store
            .station_snapshot(same_station_other_bridge.station.clone())
            .await
            .unwrap(),
        Some(same_station_other_bridge)
    );
    let mut child: ResourceRef = visible_one.station;
    child.native_protocol_reference =
        Some(uob_contracts::NativeProtocolReference::Ocpp16 { connector_id: 1 });
    assert_eq!(
        store.station_snapshot(child).await.unwrap_err().code(),
        StorageErrorCode::InvalidRequest
    );
    store
        .shutdown(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    drop(store);
    let reopened = Store::open(&file, 16).unwrap();
    assert_eq!(
        reopened
            .station_snapshot(visible_two.station.clone())
            .await
            .unwrap(),
        Some(visible_two)
    );
    reopened
        .shutdown(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    drop(reopened);
    let _ = std::fs::remove_file(file);
}

#[tokio::test]
async fn event_sequence_survives_reopen_and_conflict_rolls_back_snapshot_and_event() {
    let file = path();
    let store = Store::open(&file, 16).unwrap();
    let initial = snapshot("bridge-a", "station-a");
    assert_eq!(store.reserve_event_sequence().await.unwrap(), 1);
    commit(&store, initial.clone()).await;
    let event = EventEnvelope {
        event_id: EventId::new("bridge-a/event/10").unwrap(),
        schema_version: initial.schema_version,
        runtime: serde_json::from_value(serde_json::json!({
            "environment":"demo", "release_id":"test", "release_digest":"sha256:test",
            "process_instance_id":"test-process"
        }))
        .unwrap(),
        resource: initial.station.clone(),
        source_time: None,
        observed_at: initial.observed_at,
        event_type: EventType::new("station.snapshot.changed").unwrap(),
        origin: EventOrigin::Station,
        sequence: 10,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::StationSnapshot(initial.clone()),
    };
    let mut write = AtomicStoreWrite::<(), StationEvent, TransactionSnapshot, ()>::empty();
    write.journal_events.push(event.clone());
    store.write_atomic(write).await.unwrap();
    let mut changed = initial.clone();
    changed.connectivity = Connectivity::Disconnected;
    let mut conflicting = AtomicStoreWrite::<(), StationEvent, TransactionSnapshot, ()>::empty();
    conflicting.station_snapshot = Some(changed);
    conflicting.journal_events.push(event);
    assert_eq!(
        store.write_atomic(conflicting).await.unwrap_err().code(),
        StorageErrorCode::Conflict
    );
    store
        .shutdown(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    drop(store);
    let reopened = Store::open(&file, 16).unwrap();
    assert_eq!(
        reopened
            .station_snapshot(initial.station.clone())
            .await
            .unwrap(),
        Some(initial)
    );
    assert_eq!(reopened.reserve_event_sequence().await.unwrap(), 11);
    reopened
        .shutdown(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    drop(reopened);
    let _ = std::fs::remove_file(file);
}
