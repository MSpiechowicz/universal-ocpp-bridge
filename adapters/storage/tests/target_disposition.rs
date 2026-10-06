use std::path::PathBuf;

use rusqlite::Connection;
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, DeliveryDispositionAction, DeliveryDispositionRequest,
    DeliveryDispositionState, DeliveryId, Durability, OperationalStore, PageLimit, PendingDelivery,
    PendingDeliveryQuery, StorageErrorCode, StorageWritePurpose, TargetBacklogFact,
    TargetDeliveryDestination, TargetDeliveryStore, TargetDispositionStore,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, ContractVersion, Environment, EventEnvelope, EventId, EventOrigin,
    EventType, PrincipalId, ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity, StationId,
    TargetInstanceId, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;

struct Database(PathBuf);

impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-disposition-{}.sqlite3", Uuid::new_v4())))
    }

    fn open(&self) -> Store {
        Store::open(&self.0, 8).expect("open store")
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _database = std::fs::remove_file(&self.0);
        let _wal = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _shared_memory = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

fn destination(target: &str, revision: u64) -> TargetDeliveryDestination {
    TargetDeliveryDestination {
        target_instance_id: TargetInstanceId::new(target).unwrap(),
        configuration_revision: revision,
    }
}

fn timestamp(minute: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::October, 6).unwrap(),
            Time::from_hms(9, minute, 0).unwrap(),
        )
        .assume_offset(UtcOffset::UTC),
    )
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-test").unwrap(),
        station_id: StationId::new("station-test").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

fn event(event_id: &str, sequence: u64) -> EventEnvelope<String> {
    EventEnvelope {
        event_id: EventId::new(event_id).unwrap(),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("release-test").unwrap(),
            release_digest: ArtifactDigest::new("sha256:test").unwrap(),
            process_instance_id: ProcessInstanceId::new("process-test").unwrap(),
        },
        resource: resource(),
        source_time: None,
        observed_at: timestamp(0),
        event_type: EventType::new("test.event.v1").unwrap(),
        origin: EventOrigin::Bridge,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: "test event".to_owned(),
    }
}

async fn seed(
    store: &Store,
    event_id: &str,
    sequence: u64,
    owner: &TargetDeliveryDestination,
    durability: Durability,
) {
    let delivery = PendingDelivery {
        delivery_id: DeliveryId::new(format!("delivery-{event_id}")).unwrap(),
        event_id: EventId::new(event_id).unwrap(),
        target_instance_id: owner.target_instance_id.clone(),
        target_configuration_revision: owner.configuration_revision,
        ordering_key: resource(),
        deadline: timestamp(59),
        durability,
        payload: format!("payload {event_id}"),
    };
    store
        .write_atomic(AtomicStoreWrite {
            charging_profile_201: None,
            reservation_16: None,
            reservation_observations_16: Vec::new(),
            reservation_201: None,
            reservation_observations_201: Vec::new(),
            firmware_16: None,
            firmware_observations_16: Vec::new(),
            purpose: StorageWritePurpose::Routine,
            station_snapshot: None,
            authorization_changes: Vec::new(),
            command: None,
            command_result: None,
            journal_events: vec![event(event_id, sequence)],
            required_deliveries: vec![delivery],
            committed_records: Vec::new(),
        })
        .await
        .expect("seed delivery");
}

fn request(
    owner: &TargetDeliveryDestination,
    action: DeliveryDispositionAction,
) -> DeliveryDispositionRequest {
    DeliveryDispositionRequest {
        destination: owner.clone(),
        action,
        principal_id: PrincipalId::new("operator-a").unwrap(),
        authorized_at: timestamp(10),
    }
}

async fn pending_for(store: &Store, owner: &TargetDeliveryDestination) -> usize {
    store
        .read_pending_deliveries(PendingDeliveryQuery {
            target_instance_id: owner.target_instance_id.clone(),
            target_configuration_revision: owner.configuration_revision,
            ready_at: timestamp(30),
            limit: PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn backlog_groups_critical_and_best_effort_work_by_exact_destination() {
    let database = Database::new();
    let store = database.open();
    let old = destination("main", 1);
    let newer = destination("main", 2);
    seed(&store, "a", 1, &old, Durability::Critical).await;
    seed(&store, "b", 2, &old, Durability::BestEffortTelemetry).await;
    seed(&store, "c", 3, &newer, Durability::BestEffortTelemetry).await;

    assert_eq!(
        store.target_delivery_backlog().await.unwrap(),
        vec![
            TargetBacklogFact {
                destination: old,
                pending_critical_deliveries: 1,
                pending_deliveries: 2,
            },
            TargetBacklogFact {
                destination: newer,
                pending_critical_deliveries: 0,
                pending_deliveries: 1,
            },
        ]
    );
}

#[tokio::test]
async fn authorization_requires_pending_critical_work_and_one_unsettled_record_per_owner() {
    let database = Database::new();
    let store = database.open();
    let owner = destination("main", 1);
    let telemetry_only = destination("main", 2);
    seed(&store, "a", 1, &owner, Durability::Critical).await;
    seed(
        &store,
        "b",
        2,
        &telemetry_only,
        Durability::BestEffortTelemetry,
    )
    .await;

    let error = store
        .authorize_target_disposition(request(&telemetry_only, DeliveryDispositionAction::Archive))
        .await
        .unwrap_err();
    assert_eq!(error.code(), StorageErrorCode::Conflict);

    let record = store
        .authorize_target_disposition(request(&owner, DeliveryDispositionAction::Archive))
        .await
        .unwrap();
    assert!(record.audit_event_id.starts_with("disposition-"));
    assert_eq!(record.state, DeliveryDispositionState::Authorized);
    assert_eq!(record.principal_id.as_str(), "operator-a");
    let duplicate = store
        .authorize_target_disposition(request(&owner, DeliveryDispositionAction::Discard))
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), StorageErrorCode::Conflict);

    assert_eq!(
        store.pending_target_dispositions().await.unwrap(),
        vec![record]
    );
    // Authorization alone never removes work: the old destination may still be running.
    assert_eq!(pending_for(&store, &owner).await, 1);
}

#[tokio::test]
async fn start_up_archives_or_discards_only_unselected_owners_under_their_audit_event() {
    let database = Database::new();
    let store = database.open();
    let archived = destination("mqtt-main", 3);
    let discarded = destination("legacy", 1);
    let selected = destination("http-main", 1);
    seed(&store, "a", 1, &archived, Durability::Critical).await;
    seed(&store, "b", 2, &archived, Durability::BestEffortTelemetry).await;
    seed(&store, "c", 3, &discarded, Durability::Critical).await;
    seed(&store, "d", 4, &selected, Durability::Critical).await;
    let archive = store
        .authorize_target_disposition(request(&archived, DeliveryDispositionAction::Archive))
        .await
        .unwrap();
    let discard = store
        .authorize_target_disposition(request(&discarded, DeliveryDispositionAction::Discard))
        .await
        .unwrap();
    // Work admitted after authorization belongs to the same audited owner.
    seed(&store, "e", 5, &archived, Durability::Critical).await;

    let settled = store
        .settle_target_dispositions(Some(selected.clone()), timestamp(20))
        .await
        .unwrap();
    assert_eq!(settled.len(), 2);
    assert_eq!(settled[0].audit_event_id, archive.audit_event_id);
    assert_eq!(settled[0].state, DeliveryDispositionState::Executed);
    assert_eq!(settled[0].critical_deliveries, Some(2));
    assert_eq!(settled[0].deliveries, Some(3));
    assert_eq!(settled[0].settled_at, Some(timestamp(20)));
    assert_eq!(settled[1].audit_event_id, discard.audit_event_id);
    assert_eq!(settled[1].critical_deliveries, Some(1));

    assert_eq!(pending_for(&store, &archived).await, 0);
    assert_eq!(pending_for(&store, &discarded).await, 0);
    assert_eq!(
        pending_for(&store, &selected).await,
        1,
        "selected work is untouched"
    );
    assert!(
        store
            .pending_target_dispositions()
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);

    let connection = Connection::open(&database.0).unwrap();
    let archived_rows: Vec<(String, String)> = connection
        .prepare("SELECT audit_event_id, delivery_id FROM target_delivery_archive ORDER BY row_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        archived_rows,
        ["delivery-a", "delivery-b", "delivery-e"]
            .map(|id| (archive.audit_event_id.clone(), id.to_owned()))
            .to_vec()
    );
    let audit: Vec<(String, String)> = connection
        .prepare("SELECT action, state FROM target_delivery_dispositions ORDER BY row_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        audit,
        vec![
            ("archive".to_owned(), "executed".to_owned()),
            ("discard".to_owned(), "executed".to_owned()),
        ]
    );
}

#[tokio::test]
async fn start_up_that_still_selects_the_owner_supersedes_the_authorization() {
    let database = Database::new();
    let store = database.open();
    let owner = destination("main", 1);
    seed(&store, "a", 1, &owner, Durability::Critical).await;
    store
        .authorize_target_disposition(request(&owner, DeliveryDispositionAction::Discard))
        .await
        .unwrap();

    let settled = store
        .settle_target_dispositions(Some(owner.clone()), timestamp(20))
        .await
        .unwrap();
    assert_eq!(settled[0].state, DeliveryDispositionState::Superseded);
    assert_eq!(settled[0].deliveries, None);
    assert_eq!(pending_for(&store, &owner).await, 1);

    // A superseded authorization no longer blocks a fresh one for the same owner.
    store
        .authorize_target_disposition(request(&owner, DeliveryDispositionAction::Archive))
        .await
        .unwrap();
    let reopened = {
        drop(store);
        database.open()
    };
    assert_eq!(
        reopened.pending_target_dispositions().await.unwrap().len(),
        1
    );
}
