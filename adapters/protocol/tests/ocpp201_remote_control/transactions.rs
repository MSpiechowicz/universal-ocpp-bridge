use super::support::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::remote_control::RemoteControlStore;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201::remote_control::observation::transaction_effect;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn committed_transactions_link_remote_id_and_stop_native_id_across_restart() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::Control],
    ));
    let start = command(
        &snapshot,
        "remote-start",
        CommandOperation::Start {
            authorization_reference: Some(reference().await.as_str().to_owned()),
        },
    );
    let submit = {
        let commands = commands.clone();
        let start = start.clone();
        tokio::spawn(async move { commands.submit(start).await.unwrap() })
    };
    assert_eq!(
        receive_json(&mut running.peer).await,
        fixture("remote-start")
    );
    let allocation = store
        .remote_control_evidence(start.request.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(allocation.remote_start_id, Some(1));
    assert!(allocation.response_status.is_none());
    running.peer.send_text(json!([3,"remote-start",{"status":"Accepted","transactionId":"independent-transaction-001","statusInfo":{"reasonCode":"Accepted","additionalInfo":"private token must not persist"}}]).to_string()).await.unwrap();
    let result = submit.await.unwrap();
    accepted(&result);
    assert!(result.observed_effects.is_empty());
    assert!(persisted(&store).await.transactions.is_empty());
    let evidence = store
        .remote_control_evidence(start.request.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        evidence.native_transaction_id.as_deref(),
        Some("independent-transaction-001")
    );
    assert!(
        !serde_json::to_string(&evidence)
            .unwrap()
            .contains("private")
    );
    let mut frame = fixture("transaction-started");
    frame[3]["timestamp"] = json!("2026-09-01T02:00:00Z");
    frame[3]["triggerReason"] = json!("RemoteStart");
    frame[3]["transactionInfo"]["remoteStartId"] = json!(1);
    let event = transaction(&mut running, &store, &mut snapshot, frame, 1).await;
    port.update_committed(snapshot.clone()).unwrap();
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Pending
    );
    let durable = store
        .command_by_request_id(start.request.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    let effect = transaction_effect(&durable, &event, &evidence).unwrap();
    let linked = coordinator
        .reconcile_observed_effect(durable.request_id.clone(), effect.clone())
        .await
        .unwrap()
        .unwrap();
    accepted(&linked);
    assert_eq!(linked.observed_effects.len(), 1);
    assert_eq!(
        coordinator
            .reconcile_observed_effect(durable.request_id.clone(), effect)
            .await
            .unwrap()
            .unwrap()
            .observed_effects
            .len(),
        1
    );
    let mut unrelated = event.clone();
    unrelated
        .payload
        .protocol_state
        .as_mut()
        .unwrap()
        .remote_start_id = Some(2);
    assert!(transaction_effect(&durable, &unrelated, &evidence).is_none());
    let mut unrelated = event.clone();
    unrelated
        .payload
        .protocol_state
        .as_mut()
        .unwrap()
        .native_transaction_id = "another".to_owned();
    assert!(transaction_effect(&durable, &unrelated, &evidence).is_none());
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let store = database.open();
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let (_, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    // Restore the committed transaction as the station owner would on reconnect.
    port.update_committed(snapshot.clone()).unwrap();
    assert_eq!(
        store
            .remote_control_evidence(durable.request_id.clone())
            .await
            .unwrap(),
        Some(evidence.clone())
    );
    assert_eq!(
        store
            .reserve_remote_start(durable.request_id.clone())
            .await
            .unwrap(),
        1
    );
    assert_eq!(coordinator.submit(start).await.unwrap(), linked);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    let stop = command(
        &snapshot,
        "remote-stop",
        CommandOperation::Stop {
            transaction_id: snapshot.transactions[0].transaction_id.clone(),
        },
    );
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::Control],
    ));
    for status in ["rejected", "accepted"] {
        let mut stop = stop.clone();
        stop.request.request_id = RequestId::new(format!("stop-{status}")).unwrap();
        let submit = {
            let commands = commands.clone();
            let stop = stop.clone();
            tokio::spawn(async move { commands.submit(stop).await.unwrap() })
        };
        let mut expected = fixture("remote-stop");
        expected[1] = json!(stop.request.request_id);
        assert_eq!(receive_json(&mut running.peer).await, expected);
        let mut response = fixture(&format!("remote-stop-{status}"));
        response[1] = expected[1].clone();
        running.peer.send_text(response.to_string()).await.unwrap();
        let result = submit.await.unwrap();
        assert!(
            matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == (status == "accepted"))
        );
        assert!(result.observed_effects.is_empty());
    }
    let mut frame = fixture("transaction-updated");
    frame[3]["timestamp"] = json!("2026-09-01T02:01:00Z");
    transaction(&mut running, &store, &mut snapshot, frame, 2).await;
    assert_eq!(
        persisted(&store).await.transactions[0]
            .protocol_state
            .as_ref()
            .unwrap()
            .remote_start_id,
        Some(1)
    );
    let mut frame = fixture("transaction-ended");
    frame[3]["timestamp"] = json!("2026-09-01T02:02:00Z");
    let event = transaction(&mut running, &store, &mut snapshot, frame, 3).await;
    let stop = store
        .command_by_request_id(RequestId::new("stop-accepted").unwrap())
        .await
        .unwrap()
        .unwrap();
    let effect = transaction_effect(&stop, &event, &RemoteControlEvidence::default()).unwrap();
    let linked = coordinator
        .reconcile_observed_effect(stop.request_id, effect)
        .await
        .unwrap()
        .unwrap();
    accepted(&linked);
    assert_eq!(linked.observed_effects.len(), 1);
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Ended
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn transaction(
    running: &mut RunningSession,
    store: &Store,
    snapshot: &mut StationSnapshot,
    frame: Value,
    sequence: u64,
) -> EventEnvelope<TransactionSnapshot> {
    running.peer.send_text(frame.to_string()).await.unwrap();
    let incoming = running.outputs.incoming.receive().await.unwrap();
    let ChargerObservation::TransactionEvent(observation) = incoming.call.observation else {
        panic!("transaction")
    };
    let mut updated = snapshot.clone();
    assert_eq!(
        apply_transaction_event(&mut updated, &observation, observation.occurred_at).unwrap(),
        TransactionApplyOutcome::Applied
    );
    let payload = updated.transactions[0].clone();
    let event: EventEnvelope<TransactionSnapshot> = serde_json::from_value(json!({
        "event_id":format!("event-{sequence}"),"schema_version":{"major":1,"revision":0},
        "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test","process_instance_id":"test"},
        "resource":payload.resource,"observed_at":observation.occurred_at,"event_type": match observation.event { TransactionEventKind::Started => "transaction.started", TransactionEventKind::Updated => "transaction.updated", TransactionEventKind::Ended => "transaction.ended" },
        "origin":{"kind":"station"},"sequence":sequence,"payload":payload
    })).unwrap();
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(updated.clone());
    write.journal_events.push(event.clone());
    store.write_atomic(write).await.unwrap();
    *snapshot = updated;
    incoming.responder.respond(&json!({})).unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3, frame[1], {}])
    );
    let events = store
        .read_retained_events(RetainedEventQuery {
            resource: event.resource.clone(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(events.events.last(), Some(&event));
    event
}
