use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::*;
use uob_application::{
    AtomicStoreWrite, ScopedTargetQueryPort, SnapshotQuery, TargetQueryPermission, TargetQueryPort,
    TargetResourceScope,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, CanonicalConnectorId, CanonicalResource, Connectivity,
    ContractVersion, Environment, EventId, EventOrigin, EventType, ProcessInstanceId, ReleaseId,
    ResourceCapabilities, RuntimeIdentity, StationId, StationSnapshot, TargetInstanceId,
    UtcTimestamp,
};
use uuid::Uuid;

struct TestDatabase(PathBuf);
impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-management-{}.sqlite3", Uuid::new_v4())))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

fn station(name: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-test").unwrap(),
        station_id: StationId::new(name).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

fn snapshot(name: &str) -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: station(name),
        observed_at: timestamp(),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        transactions: vec![],
        current_values: vec![],
    }
}

fn timestamp() -> UtcTimestamp {
    serde_json::from_str("\"2026-09-24T00:00:00Z\"").unwrap()
}

fn authorization(names: &[&str]) -> TargetQueryAuthorization {
    TargetQueryAuthorization::new(
        TargetInstanceId::new("management-test").unwrap(),
        vec![
            TargetQueryPermission::StationSnapshots,
            TargetQueryPermission::RetainedEvents,
        ],
        names
            .iter()
            .map(|name| TargetResourceScope::Station {
                bridge_id: station(name).bridge_id,
                station_id: station(name).station_id,
            })
            .collect(),
    )
}

fn source(store: &ManagementStore) -> Arc<dyn CanonicalQuerySource<Value>> {
    Arc::new(ManagementSource::new(store.clone()))
}

async fn save_snapshot(store: &ManagementStore, name: &str) {
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot(name));
    store.write_atomic(write).await.unwrap();
}

fn event(name: &str, id: &str) -> EventEnvelope<StationEvent> {
    EventEnvelope {
        event_id: EventId::new(id).unwrap(),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("release-test").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new("process-test").unwrap(),
        },
        resource: station(name),
        source_time: None,
        observed_at: timestamp(),
        event_type: EventType::new("station.snapshot").unwrap(),
        origin: EventOrigin::Bridge,
        sequence: match id {
            "second" => 2,
            "third" => 3,
            _ => 1,
        },
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::StationSnapshot(snapshot(name)),
    }
}

async fn save_event(store: &ManagementStore, name: &str, id: &str) {
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(event(name, id));
    store.write_atomic(write).await.unwrap();
}

fn connector(name: &str, id: &str, native_id: u32) -> ResourceRef {
    ResourceRef {
        resource: Some(CanonicalResource::Connector {
            connector_id: CanonicalConnectorId::new(id).unwrap(),
        }),
        native_protocol_reference: Some(NativeProtocolReference::Ocpp16 {
            connector_id: native_id,
        }),
        ..station(name)
    }
}

fn transaction_event(name: &str, id: &str) -> EventEnvelope<StationEvent> {
    let resource = connector(name, "connector-1", 1);
    let mut event = event(name, id);
    event.resource = resource.clone();
    event.event_type = EventType::new("transaction.started").unwrap();
    event.payload = StationEvent::Transaction(
        serde_json::from_value(serde_json::json!({
            "transaction_id": format!("tx-{id}"),
            "resource": resource,
            "state": "active",
            "started_at": timestamp(),
            "ended_at": null,
            "protocol_state": null
        }))
        .unwrap(),
    );
    event
}

#[tokio::test]
async fn inventory_filters_before_limit_and_exact_detail_ignores_inventory_position() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 16).unwrap();
    for name in ["a", "b", "c", "d", "e"] {
        save_snapshot(&store, name).await;
    }
    let port = ScopedTargetQueryPort::new(source(&store), authorization(&["d", "e"]));
    let first = port
        .query(TargetQuery::StationSnapshots(SnapshotQuery {
            after: None,
            limit: one(),
        }))
        .await
        .unwrap();
    let TargetQueryResult::StationSnapshots(first) = first else {
        panic!("snapshot page expected");
    };
    assert_eq!(
        first
            .items
            .iter()
            .map(|value| &value.station)
            .collect::<Vec<_>>(),
        vec![&station("d")]
    );
    let second = port
        .query(TargetQuery::StationSnapshots(SnapshotQuery {
            after: first.next_cursor,
            limit: one(),
        }))
        .await
        .unwrap();
    let TargetQueryResult::StationSnapshots(second) = second else {
        panic!("snapshot page expected");
    };
    assert_eq!(
        second
            .items
            .iter()
            .map(|value| &value.station)
            .collect::<Vec<_>>(),
        vec![&station("e")]
    );
    assert!(second.next_cursor.is_none());
    assert_eq!(
        port.query(TargetQuery::StationSnapshot(station("e")))
            .await
            .unwrap(),
        TargetQueryResult::StationSnapshot(Some(snapshot("e"))),
    );
    let denied = port
        .query(TargetQuery::StationSnapshot(station("a")))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), TargetPortErrorCode::Unauthorized);
}
#[tokio::test]
async fn native_controller_metadata_preserves_authorized_station_identity() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 8).unwrap();
    let mut observed = snapshot("controller");
    observed.station.native_protocol_reference =
        Some(NativeProtocolReference::Ocpp16 { connector_id: 0 });
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(observed.clone());
    store.write_atomic(write).await.unwrap();
    let authorization = TargetQueryAuthorization::new(
        TargetInstanceId::new("management-test").unwrap(),
        vec![TargetQueryPermission::StationSnapshots],
        vec![TargetResourceScope::Resource(observed.station.clone())],
    );
    let port = ScopedTargetQueryPort::new(source(&store), authorization);
    assert_eq!(
        port.query(TargetQuery::StationSnapshot(station("controller")))
            .await
            .unwrap(),
        TargetQueryResult::StationSnapshot(Some(observed.clone())),
    );
    let TargetQueryResult::StationSnapshots(page) = port
        .query(TargetQuery::StationSnapshots(SnapshotQuery {
            after: None,
            limit: one(),
        }))
        .await
        .unwrap()
    else {
        panic!("snapshot page expected");
    };
    assert_eq!(page.items, vec![observed]);
}

#[tokio::test]
async fn child_only_grant_cannot_read_a_whole_station_snapshot() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 8).unwrap();
    save_snapshot(&store, "allowed").await;
    let mut child = station("allowed");
    child.resource = Some(CanonicalResource::Connector {
        connector_id: CanonicalConnectorId::new("connector-1").unwrap(),
    });
    let authorization = TargetQueryAuthorization::new(
        TargetInstanceId::new("management-test").unwrap(),
        vec![TargetQueryPermission::StationSnapshots],
        vec![TargetResourceScope::Resource(child)],
    );
    let port = ScopedTargetQueryPort::new(source(&store), authorization);
    let TargetQueryResult::StationSnapshots(page) = port
        .query(TargetQuery::StationSnapshots(SnapshotQuery {
            after: None,
            limit: one(),
        }))
        .await
        .unwrap()
    else {
        panic!("snapshot page expected");
    };
    assert!(page.items.is_empty());
    let error = port
        .query(TargetQuery::StationSnapshot(station("allowed")))
        .await
        .unwrap_err();
    assert_eq!(error.code(), TargetPortErrorCode::Unauthorized);
}

#[tokio::test]
async fn retained_page_cursor_advances_only_in_the_requested_resource_stream() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 8).unwrap();
    save_event(&store, "allowed", "first").await;
    save_event(&store, "foreign", "foreign").await;
    save_event(&store, "allowed", "second").await;
    let port = ScopedTargetQueryPort::new(source(&store), authorization(&["allowed"]));
    let page = |after| {
        TargetQuery::RetainedEvents(RetainedEventQuery {
            resource: station("allowed"),
            after,
            limit: one(),
        })
    };
    let TargetQueryResult::RetainedEvents(first) = port.query(page(None)).await.unwrap() else {
        panic!("retained page expected");
    };
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].event_id.as_str(), "first");
    let TargetQueryResult::RetainedEvents(second) =
        port.query(page(first.next_cursor)).await.unwrap()
    else {
        panic!("retained page expected");
    };
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].event_id.as_str(), "second");
    assert!(second.next_cursor.is_none());
    assert_eq!(second.items[0].resource, station("allowed"));
}

#[tokio::test]
async fn linked_native_child_event_replays_through_management_page_and_subscription() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 8).unwrap();
    let original = transaction_event("allowed", "first");
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(original.clone());
    store.write_atomic(write).await.unwrap();

    let port = ScopedTargetQueryPort::new(source(&store), authorization(&["allowed"]));
    let mut resource = original.resource.clone();
    resource.native_protocol_reference = None;
    let query = |resource, after| RetainedEventQuery {
        resource,
        after,
        limit: one(),
    };
    let TargetQueryResult::RetainedEvents(page) = port
        .query(TargetQuery::RetainedEvents(query(resource.clone(), None)))
        .await
        .unwrap()
    else {
        panic!("retained page expected");
    };
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].resource, original.resource);

    let mut sibling = connector("allowed", "connector-2", 2);
    sibling.native_protocol_reference = None;
    let TargetQueryResult::RetainedEvents(sibling_page) = port
        .query(TargetQuery::RetainedEvents(query(sibling.clone(), None)))
        .await
        .unwrap()
    else {
        panic!("retained page expected");
    };
    assert!(sibling_page.items.is_empty());

    let mut stream = port
        .subscribe_retained_events(query(resource.clone(), None))
        .await
        .unwrap();
    let first = std::future::poll_fn(|cx| stream.as_mut().poll_event(cx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.event.resource, original.resource);
    let next = transaction_event("allowed", "second");
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(next.clone());
    store.write_atomic(write).await.unwrap();
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        std::future::poll_fn(|cx| stream.as_mut().poll_event(cx)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(second.event.resource, next.resource);
    assert_eq!(second.event.event_id, next.event_id);
    assert_ne!(first.cursor, second.cursor);
    let TargetQueryResult::RetainedEvents(resumed) = port
        .query(TargetQuery::RetainedEvents(query(
            resource,
            Some(first.cursor.clone()),
        )))
        .await
        .unwrap()
    else {
        panic!("retained page expected");
    };
    assert_eq!(resumed.items.len(), 1);
    assert_eq!(resumed.items[0].event_id, next.event_id);
    let wrong_cursor = port
        .query(TargetQuery::RetainedEvents(query(
            sibling,
            Some(first.cursor),
        )))
        .await
        .unwrap_err();
    assert_eq!(wrong_cursor.code(), TargetPortErrorCode::CursorExpired);
}

#[tokio::test]
async fn cursor_expiry_fails_before_subscription_and_commits_arrive_with_exact_checkpoint() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 16).unwrap();
    save_event(&store, "allowed", "first").await;
    save_event(&store, "other", "foreign").await;
    let port = ScopedTargetQueryPort::new(source(&store), authorization(&["allowed"]));
    let resource = station("allowed");
    let query = |after| RetainedEventQuery {
        resource: resource.clone(),
        after,
        limit: one(),
    };
    let expired = RetainedEventCursor::new("uob:event:999999").unwrap();
    match port.subscribe_retained_events(query(Some(expired))).await {
        Ok(_) => panic!("stale cursor must fail before subscription starts"),
        Err(error) => assert_eq!(error.code(), TargetPortErrorCode::CursorExpired),
    }
    let foreign_cursor = store
        .read_retained_events(RetainedEventQuery {
            resource: station("other"),
            after: None,
            limit: one(),
        })
        .await
        .unwrap()
        .resume_cursor
        .unwrap();
    match port
        .subscribe_retained_events(query(Some(foreign_cursor)))
        .await
    {
        Ok(_) => panic!("cursor for another station must not resume this stream"),
        Err(error) => assert_eq!(error.code(), TargetPortErrorCode::CursorExpired),
    }
    let mut stream = port.subscribe_retained_events(query(None)).await.unwrap();
    assert_eq!(stream.capacity(), 1);
    assert_eq!(stream.backlog(), 1);
    let first = std::future::poll_fn(|cx| stream.as_mut().poll_event(cx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.event.resource, resource);
    assert_eq!(first.event.event_id.as_str(), "first");
    assert_eq!(
        first.event.payload,
        serde_json::to_value(snapshot("allowed")).unwrap()
    );
    assert!(first.cursor.as_str().starts_with("uob:event:"));
    let idle = tokio::time::timeout(
        Duration::from_millis(30),
        std::future::poll_fn(|cx| stream.as_mut().poll_event(cx)),
    )
    .await;
    assert!(
        idle.is_err(),
        "an empty retained stream must wait, not finish or spin"
    );
    save_event(&store, "allowed", "second").await;
    let next = tokio::time::timeout(
        Duration::from_secs(2),
        std::future::poll_fn(|cx| stream.as_mut().poll_event(cx)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(next.event.event_id.as_str(), "second");
    assert_ne!(first.cursor, next.cursor);
    drop(stream);
    save_event(&store, "allowed", "third").await;
    let mut resumed = port
        .subscribe_retained_events(query(Some(next.cursor)))
        .await
        .unwrap();
    let third = std::future::poll_fn(|cx| resumed.as_mut().poll_event(cx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.event.event_id.as_str(), "third");
    assert_eq!(third.event.resource, resource);
}
