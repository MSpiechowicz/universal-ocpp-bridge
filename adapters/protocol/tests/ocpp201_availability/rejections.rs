use super::support::*;
use serde_json::json;
use std::time::Duration;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201;

#[tokio::test]
async fn invalid_unsupported_unprivileged_and_expired_requests_never_reach_wire() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = open(&database);
    let (snapshot, port, coordinator) = setup(&store, running.handle.clone()).await;
    for (index, payload) in [
        json!({"evse":{"id":0,"connectorId":1},"operationalStatus":"Inoperative"}),
        json!({"evse":{"id":1,"connectorId":2},"operationalStatus":"Inoperative"}),
        json!({"evse":{"id":1,"connectorId":-1},"operationalStatus":"Inoperative"}),
        json!({"evse":{"id":1,"connectorId":1},"operationalStatus":"Wrong"}),
        json!({"evse":{"id":1,"connectorId":1},"operationalStatus":null}),
        json!({"evse":{"id":1,"connectorId":1},"operationalStatus":"Inoperative","extra":true}),
        json!({"operationalStatus":"Operative"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut external = command(&snapshot, &format!("invalid-{index}"), 1, "Inoperative");
        external.request.operation = protocol("ChangeAvailability", payload);
        let result = coordinator.submit(external).await.unwrap();
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::Rejected { .. }
        ));
    }
    let readonly = scoped(coordinator.clone(), &snapshot, vec![AccessPermission::Read]);
    assert!(
        readonly
            .submit(command(&snapshot, "readonly", 1, "Inoperative"))
            .await
            .is_err()
    );
    let normal = scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::Control],
    );
    assert!(
        normal
            .submit(command(&snapshot, "normal", 1, "Inoperative"))
            .await
            .is_err()
    );
    let mut expired = command(&snapshot, "expired", 1, "Inoperative");
    expired.request.expires_at = Clock.now();
    assert!(matches!(
        coordinator.submit(expired).await.unwrap().lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    let mut unsupported = snapshot.clone();
    unsupported.resources[0].capabilities.operations.clear();
    port.update_committed(unsupported).unwrap();
    assert!(matches!(
        coordinator
            .submit(command(&snapshot, "unsupported", 1, "Inoperative"))
            .await
            .unwrap()
            .lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn status_failure_is_atomic_and_source_time_cannot_confirm_an_old_observation() {
    let running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = open(&database);
    let (mut snapshot, _, coordinator) = setup(&store, running.handle.clone()).await;
    let before = snapshot.clone();
    let invalid = json!([2,"invalid","StatusNotification",{"evseId":1,"connectorId":99,"connectorStatus":"Unavailable","timestamp":"2026-09-01T02:00:00Z"}]);
    let evidence_context = context(&snapshot, 1);
    assert!(
        v201::availability::complete_status(
            v201::decode_call(invalid.to_string().as_bytes()).unwrap(),
            &store,
            &mut snapshot,
            evidence_context,
            Clock.now()
        )
        .await
        .is_err()
    );
    assert_eq!(persisted(&store).await, before);
    // Journal identity collision rolls back the snapshot update in the same SQLite transaction.
    let status = json!([2,"status","StatusNotification",{"evseId":1,"connectorId":1,"connectorStatus":"Unavailable","timestamp":"2020-01-01T00:00:00Z"}]);
    let evidence_context = context(&snapshot, 1);
    v201::availability::complete_status(
        v201::decode_call(status.to_string().as_bytes()).unwrap(),
        &store,
        &mut snapshot,
        evidence_context,
        Clock.now(),
    )
    .await
    .unwrap();
    let durable = command(&snapshot, "stale-evidence", 1, "Inoperative").admit(Clock.now());
    assert!(
        v201::availability::observed_effect(&durable, &event(&store, &snapshot).await).is_none()
    );
    let before = snapshot.clone();
    let mut changed = status;
    changed[3]["connectorStatus"] = json!("Available");
    changed[3]["timestamp"] = json!("2026-09-01T02:00:00Z");
    let evidence_context = context(&snapshot, 1);
    assert!(
        v201::availability::complete_status(
            v201::decode_call(changed.to_string().as_bytes()).unwrap(),
            &store,
            &mut snapshot,
            evidence_context,
            Clock.now()
        )
        .await
        .is_err()
    );
    assert_eq!(snapshot, before);
    assert_eq!(persisted(&store).await, before);
    drop(coordinator);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let evidence_context = context(&snapshot, 2);
    assert!(
        v201::availability::complete_status(
            v201::decode_call(changed.to_string().as_bytes()).unwrap(),
            &store,
            &mut snapshot,
            evidence_context,
            Clock.now()
        )
        .await
        .is_err()
    );
    assert_eq!(snapshot, before);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
}
