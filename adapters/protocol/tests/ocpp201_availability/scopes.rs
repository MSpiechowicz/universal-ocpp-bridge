use super::support::*;
use serde_json::json;
use std::time::Duration;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201::availability::observed_effect;

#[tokio::test]
async fn evse_scope_preserves_connector_states_and_never_controls_another_evse() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (mut snapshot, port, coordinator) = setup(&store, running.handle.clone()).await;
    let mut external = command(&snapshot, "evse", 1, "Inoperative");
    external.request.resource = snapshot.resources[3].resource.clone();
    external.request.operation = protocol(
        "ChangeAvailability",
        fixture("availability-evse")[3].clone(),
    );
    let commands = scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    );
    let result = exchange(&mut running, commands.clone(), external.clone(), "Accepted").await;
    let durable = store
        .command_by_request_id(external.request.request_id)
        .await
        .unwrap()
        .unwrap();
    status(&mut running, &store, &mut snapshot, 1, "Unavailable", 1).await;
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    status(&mut running, &store, &mut snapshot, 2, "Unavailable", 2).await;
    let effect = observed_effect(&durable, &event(&store, &snapshot).await).unwrap();
    assert_eq!(
        snapshot.resources[2].availability,
        AvailabilityState::Available
    );
    assert_eq!(
        coordinator
            .reconcile_observed_effect(durable.request_id, effect)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        result.lifecycle
    );

    // Restoring an EVSE does not override an independently inoperative connector.
    port.update_committed(snapshot.clone()).unwrap();
    let mut restore = command(&snapshot, "restore-evse", 1, "Operative");
    restore.request.resource = snapshot.resources[3].resource.clone();
    restore.request.operation = protocol(
        "ChangeAvailability",
        json!({"evse":{"id":1},"operationalStatus":"Operative"}),
    );
    exchange(&mut running, commands, restore.clone(), "Accepted").await;
    status(&mut running, &store, &mut snapshot, 2, "Reserved", 3).await;
    assert_eq!(
        snapshot.resources[0].availability,
        AvailabilityState::Unavailable
    );
    assert_eq!(
        snapshot.resources[1].availability,
        AvailabilityState::Occupied
    );
    let durable = store
        .command_by_request_id(restore.request.request_id)
        .await
        .unwrap()
        .unwrap();
    // Connector-only reports cannot prove the parent operational state; retain uncertainty.
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    status(&mut running, &store, &mut snapshot, 1, "Faulted", 4).await;
    assert_eq!(
        snapshot.resources[0].availability,
        AvailabilityState::Faulted
    );
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn nested_scope_null_zero_unknown_and_cross_evse_requests_are_rejected() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (snapshot, _, coordinator) = setup(&store, running.handle.clone()).await;
    for (i, evse) in [
        json!(null),
        json!({"id":1}),
        json!({"id":2,"connectorId":1}),
        json!({"id":1,"connectorId":0}),
        json!({"id":1,"connectorId":null}),
        json!({"id":1,"connectorId":1,"extra":true}),
        json!({"id":2_147_483_648_u64,"connectorId":1}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut external = command(&snapshot, &format!("nested-{i}"), 1, "Operative");
        external.request.operation = protocol(
            "ChangeAvailability",
            json!({"evse":evse,"operationalStatus":"Operative"}),
        );
        assert!(matches!(
            coordinator.submit(external).await.unwrap().lifecycle,
            CommandLifecycle::Rejected { .. }
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
