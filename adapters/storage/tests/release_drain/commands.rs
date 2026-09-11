use super::*;
use std::time::Duration as Window;
use uob_application::{
    StorageAdmissionState,
    release_drain::{ReleaseDrainPort, ReleaseJobKind},
};
use uob_contracts::{
    Connectivity, ResourceCapabilities, StationSnapshot, TransactionSnapshot, TransactionState,
};

#[tokio::test]
async fn management_and_second_target_cannot_start_but_existing_commands_can_finish() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    let prior = command("prior", "station-a", start(None), 0);
    store
        .write_atomic(command_write(prior.clone()))
        .await
        .unwrap();
    let id = store.begin_drain(Window::from_secs(2)).await.unwrap();
    let second = store.clone();
    let mut target = command("target", "station-b", start(None), 0);
    target.origin = AuthenticatedCommandOrigin::Target {
        target_instance_id: uob_contracts::TargetInstanceId::new("mqtt").unwrap(),
        principal_id: PrincipalId::new("operator-2").unwrap(),
    };
    for (writer, value) in [
        (&store, command("management", "station-a", start(None), 0)),
        (&second, target),
    ] {
        let request = value.request_id.clone();
        assert_eq!(
            writer
                .write_atomic(command_write(value))
                .await
                .unwrap_err()
                .code(),
            StorageErrorCode::Busy
        );
        assert!(
            writer
                .command_by_request_id(request)
                .await
                .unwrap()
                .is_none()
        );
    }
    let busy = store.observe_drain(id.clone()).await.unwrap();
    assert_eq!(busy.unresolved_commands, 1);
    assert!(store.seal_drain(busy).await.is_err());
    let mut completed = AtomicStoreWrite::empty();
    completed.command_result = Some(result(&prior, accepted(), 1));
    second.write_atomic(completed).await.unwrap();
    let idle = store.observe_drain(id.clone()).await.unwrap();
    assert!(idle.is_idle());
    store.seal_drain(idle).await.unwrap();
    assert!(store.write_atomic(AtomicStoreWrite::empty()).await.is_err());
    store.cancel_drain(id).await.unwrap();
    store
        .write_atomic(command_write(command("after", "station-a", start(None), 1)))
        .await
        .unwrap();
}

#[tokio::test]
async fn late_transaction_and_late_job_invalidate_idle_even_if_work_finishes_again() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    let id = store.begin_drain(Window::from_secs(2)).await.unwrap();
    let stale = store.observe_drain(id.clone()).await.unwrap();
    store
        .write_atomic(snapshot_write(TransactionState::Active))
        .await
        .unwrap();
    assert_eq!(
        store
            .observe_drain(id.clone())
            .await
            .unwrap()
            .active_transactions,
        1
    );
    assert!(store.seal_drain(stale).await.is_err());
    store
        .write_atomic(snapshot_write(TransactionState::Ended))
        .await
        .unwrap();
    let stale = store.observe_drain(id.clone()).await.unwrap();
    store
        .start_release_job("late-certificate".into(), ReleaseJobKind::Certificate)
        .await
        .unwrap();
    assert_eq!(
        store.observe_drain(id.clone()).await.unwrap().stateful_jobs,
        1
    );
    store
        .finish_release_job("late-certificate".into())
        .await
        .unwrap();
    assert!(
        store.seal_drain(stale).await.is_err(),
        "completed late work still invalidates old evidence"
    );
    store
        .seal_drain(store.observe_drain(id).await.unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn deadline_reopens_admission_without_removing_work_and_old_owner_cannot_cancel_new_drain() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    store
        .write_atomic(snapshot_write(TransactionState::Active))
        .await
        .unwrap();
    let old = store.begin_drain(Window::from_millis(25)).await.unwrap();
    assert_eq!(
        store
            .storage_retention_status()
            .await
            .unwrap()
            .new_session_admission,
        StorageAdmissionState::ReleaseDraining
    );
    tokio::time::sleep(Window::from_millis(40)).await;
    assert!(store.observe_drain(old.clone()).await.is_err());
    assert_eq!(
        store
            .storage_retention_status()
            .await
            .unwrap()
            .new_session_admission,
        StorageAdmissionState::Available
    );
    store
        .write_atomic(command_write(command(
            "after-deadline",
            "station-b",
            start(None),
            1,
        )))
        .await
        .unwrap();
    let new = store.begin_drain(Window::from_secs(2)).await.unwrap();
    store.cancel_drain(old).await.unwrap();
    let inventory = store.observe_drain(new).await.unwrap();
    assert_eq!(inventory.active_transactions, 1);
    assert_eq!(inventory.unresolved_commands, 1);
    assert!(
        store
            .write_atomic(command_write(command(
                "still-blocked",
                "station-b",
                start(None),
                1
            )))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn recovery_retains_jobs_and_transactions_but_never_restores_an_idle_permit() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    store
        .write_atomic(snapshot_write(TransactionState::Active))
        .await
        .unwrap();
    store
        .start_release_job("firmware/1".into(), ReleaseJobKind::Firmware)
        .await
        .unwrap();
    let old = store.begin_drain(Window::from_secs(2)).await.unwrap();
    store.shutdown(Window::from_secs(1)).await.unwrap();
    let reopened = Store::open(database.path(), 8).unwrap();
    assert!(reopened.observe_drain(old).await.is_err());
    let new = reopened.begin_drain(Window::from_secs(2)).await.unwrap();
    let inventory = reopened.observe_drain(new).await.unwrap();
    assert_eq!(inventory.active_transactions, 1);
    assert_eq!(inventory.stateful_jobs, 1);
    assert!(reopened.seal_drain(inventory).await.is_err());
}

#[tokio::test]
async fn privileged_start_cannot_bypass_maintenance_and_inventory_is_bounded() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    let id = store.begin_drain(Window::from_secs(2)).await.unwrap();
    let operation = CommandOperation::Ocpp(uob_contracts::PrivilegedOcppOperation {
        protocol: uob_contracts::ProtocolEdition::Ocpp16j,
        action: uob_contracts::ProtocolActionName::new("RemoteStartTransaction").unwrap(),
        payload_schema: uob_contracts::PayloadSchemaId::new("test").unwrap(),
        payload: String::new(),
    });
    assert!(
        store
            .write_atomic(command_write(command(
                "privileged-start",
                "station-a",
                operation,
                0
            )))
            .await
            .is_err()
    );
    for n in 0..128 {
        store
            .start_release_job(format!("job/{n}"), ReleaseJobKind::Firmware)
            .await
            .unwrap();
    }
    assert!(
        store
            .start_release_job("overflow".into(), ReleaseJobKind::Firmware)
            .await
            .is_err()
    );
    assert_eq!(store.observe_drain(id).await.unwrap().stateful_jobs, 128);
}

fn snapshot_write(state: TransactionState) -> AtomicStoreWrite<String, String, String, String> {
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: resource("station-a"),
        observed_at: timestamp(1),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        current_values: vec![],
        transactions: vec![TransactionSnapshot {
            transaction_id: TransactionId::new("transaction-1").unwrap(),
            resource: resource("station-a"),
            state,
            started_at: timestamp(0),
            ended_at: (state == TransactionState::Ended).then(|| timestamp(1)),
            protocol_state: None,
            ocpp16: None,
        }],
    });
    write
}

#[tokio::test]
async fn queued_transaction_linearizes_before_seal_and_sealed_boundary_refuses_later_writes() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    let window = store.begin_drain(Window::from_secs(2)).await.unwrap();
    let observed = store.observe_drain(window.clone()).await.unwrap();
    // Submission enqueues before polling: the final idle check must see this queued report.
    let late = store.write_atomic(snapshot_write(TransactionState::Pending));
    let seal = store.seal_drain(observed);
    late.await.unwrap();
    assert!(seal.await.is_err());
    store
        .write_atomic(snapshot_write(TransactionState::Ended))
        .await
        .unwrap();
    store
        .seal_drain(store.observe_drain(window.clone()).await.unwrap())
        .await
        .unwrap();
    assert!(
        store
            .write_atomic(snapshot_write(TransactionState::Active))
            .await
            .is_err()
    );
    assert!(
        store
            .start_release_job("after-seal".into(), ReleaseJobKind::Firmware)
            .await
            .is_err()
    );
    store.cancel_drain(window).await.unwrap();
    store
        .write_atomic(snapshot_write(TransactionState::Active))
        .await
        .unwrap();
}

#[tokio::test]
async fn unparseable_persisted_station_state_fails_closed() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).unwrap();
    store.shutdown(Window::from_secs(1)).await.unwrap();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute("INSERT INTO station_snapshots VALUES ('broken', '{}')", [])
        .unwrap();
    drop(connection);
    let reopened = Store::open(database.path(), 8).unwrap();
    let id = reopened.begin_drain(Window::from_secs(1)).await.unwrap();
    assert!(reopened.observe_drain(id).await.is_err());
}
