use super::support::*;
use serde_json::json;
use std::time::Duration;
use uob_application::{remote_control::RemoteControlStore, *};
use uob_contracts::*;
use uob_protocol_adapter::v201::availability::observed_effect;

#[tokio::test]
async fn native_responses_persist_without_changing_observed_availability() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (snapshot, _, coordinator) = setup(&store, running.handle.clone()).await;
    let commands = scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    );
    for (name, connector, kind) in [
        ("availability-inoperative", 1, "Inoperative"),
        ("availability-operative", 1, "Operative"),
        ("availability-station", 0, "Inoperative"),
    ] {
        for response in ["Accepted", "Scheduled", "Rejected"] {
            let id = format!("{name}-{response}");
            let external = command(&snapshot, &id, connector, kind);
            if let CommandOperation::Ocpp(op) = &external.request.operation {
                assert_eq!(op.payload, fixture(name)[3]);
            }
            let result = exchange(&mut running, commands.clone(), external, response).await;
            assert!(
                matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == (response != "Rejected"))
            );
            assert!(result.observed_effects.is_empty());
            assert_eq!(
                store
                    .command_result_by_request_id(RequestId::new(&id).unwrap())
                    .await
                    .unwrap(),
                Some(result)
            );
            assert_eq!(
                store
                    .remote_control_evidence(RequestId::new(&id).unwrap())
                    .await
                    .unwrap()
                    .unwrap()
                    .response_status
                    .as_deref(),
                Some(response)
            );
            assert_eq!(persisted(&store).await, snapshot);
        }
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn scheduled_transaction_waits_for_completion_and_fresh_status_evidence() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (mut snapshot, port, coordinator) = setup(&store, running.handle.clone()).await;
    let transaction = TransactionSnapshot {
        ocpp16: None,
        transaction_id: TransactionId::new("availability-active").unwrap(),
        resource: snapshot.resources[0].resource.clone(),
        state: TransactionState::Active,
        started_at: Clock.now(),
        ended_at: None,
        protocol_state: None,
    };
    snapshot.transactions.push(transaction);
    snapshot.resources[0].availability = AvailabilityState::Occupied;
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot.clone());
    store.write_atomic(write).await.unwrap();
    port.update_committed(snapshot.clone()).unwrap();
    let external = command(&snapshot, "scheduled", 1, "Inoperative");
    let commands = scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    );
    let result = exchange(&mut running, commands, external.clone(), "Scheduled").await;
    assert!(result.observed_effects.is_empty());
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Active
    );
    let durable = store
        .command_by_request_id(external.request.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    status(&mut running, &store, &mut snapshot, 1, "Occupied", 1).await;
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    status(&mut running, &store, &mut snapshot, 1, "Unavailable", 2).await;
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    // Independent transaction workflow owns completion; availability must never synthesize it.
    snapshot.transactions[0].state = TransactionState::Ended;
    snapshot.transactions[0].ended_at = Some(Clock.now());
    status(&mut running, &store, &mut snapshot, 1, "Unavailable", 3).await;
    let evidence = event(&store, &snapshot).await;
    let effect = observed_effect(&durable, &evidence).unwrap();
    let linked = coordinator
        .reconcile_observed_effect(durable.request_id.clone(), effect.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(linked.lifecycle, result.lifecycle);
    assert_eq!(linked.observed_effects.len(), 1);
    assert_eq!(
        coordinator
            .reconcile_observed_effect(durable.request_id.clone(), effect)
            .await
            .unwrap()
            .unwrap(),
        linked
    );
    assert_eq!(
        store
            .remote_control_evidence(durable.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .response_status
            .as_deref(),
        Some("Scheduled")
    );
    for mode in ["old", "wrong-station", "replay", "wrong-type"] {
        let mut bad = evidence.clone();
        match mode {
            "old" => bad.observed_at = time("2020-01-01T00:00:00Z"),
            "wrong-station" => bad.resource.station_id = StationId::new("other").unwrap(),
            "replay" => bad.origin = EventOrigin::Replay,
            _ => bad.event_type = EventType::new("unrelated").unwrap(),
        }
        assert!(observed_effect(&durable, &bad).is_none());
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn station_scope_waits_for_every_connector_and_never_widens_connector_control() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (mut snapshot, port, coordinator) = setup(&store, running.handle.clone()).await;
    // Native topology has two EVSEs, three connectors, and an EVSE aggregate.
    assert_eq!(snapshot.resources.len(), 4);
    port.update_committed(snapshot.clone()).unwrap();
    let external = command(&snapshot, "station", 0, "Inoperative");
    let commands = scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    );
    exchange(&mut running, commands, external.clone(), "Accepted").await;
    let durable = store
        .command_by_request_id(external.request.request_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        snapshot.resources[0].availability,
        AvailabilityState::Available
    );
    status(&mut running, &store, &mut snapshot, 1, "Unavailable", 2).await;
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    status(&mut running, &store, &mut snapshot, 2, "Unavailable", 3).await;
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_none());
    let mut frame = fixture("availability-status");
    frame[3]["evseId"] = json!(2);
    let ctx = context(&snapshot, 4);
    uob_protocol_adapter::v201::availability::complete_status(
        uob_protocol_adapter::v201::decode_call(frame.to_string().as_bytes()).unwrap(),
        &store,
        &mut snapshot,
        ctx,
        Clock.now(),
    )
    .await
    .unwrap();
    assert!(observed_effect(&durable, &event(&store, &snapshot).await).is_some());
    assert_eq!(snapshot.resources.len(), 4);
    // Older source state is acknowledged but must not rewind durable Unavailable state.
    let before = snapshot.resources.clone();
    let frame = json!([2,"stale","StatusNotification",{"evseId":1,"connectorId":2,"connectorStatus":"Available","timestamp":"2020-01-01T00:00:00Z"}]);
    let call = uob_protocol_adapter::v201::decode_call(frame.to_string().as_bytes()).unwrap();
    let context = context(&snapshot, 5);
    uob_protocol_adapter::v201::availability::complete_status(
        call,
        &store,
        &mut snapshot,
        context,
        Clock.now(),
    )
    .await
    .unwrap();
    assert_eq!(snapshot.resources, before);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn delayed_reply_keeps_status_processing_live_and_operative_evidence_separate() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (mut snapshot, port, coordinator) = setup(&store, running.handle.clone()).await;
    status(&mut running, &store, &mut snapshot, 1, "Unavailable", 1).await;
    port.update_committed(snapshot.clone()).unwrap();
    let external = command(&snapshot, "delayed", 1, "Operative");
    let submit = {
        let coordinator = coordinator.clone();
        let external = external.clone();
        tokio::spawn(async move { coordinator.submit(external).await.unwrap() })
    };
    assert_eq!(
        receive_json(&mut running.peer).await[2],
        json!("ChangeAvailability")
    );
    assert!(!submit.is_finished());
    assert_eq!(
        store
            .command_result_by_request_id(external.request.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        CommandLifecycle::Dispatched
    );
    // Reading stays live while waiting for the reply. Occupied is operative, not charging proof.
    status(&mut running, &store, &mut snapshot, 1, "Occupied", 2).await;
    assert!(!submit.is_finished());
    running
        .peer
        .send_text(json!([3,"delayed",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let result = submit.await.unwrap();
    assert!(result.observed_effects.is_empty());
    let durable = store
        .command_by_request_id(external.request.request_id)
        .await
        .unwrap()
        .unwrap();
    let effect = observed_effect(&durable, &event(&store, &snapshot).await).unwrap();
    let linked = coordinator
        .reconcile_observed_effect(durable.request_id, effect)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(linked.lifecycle, result.lifecycle);
    assert_eq!(linked.observed_effects.len(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
