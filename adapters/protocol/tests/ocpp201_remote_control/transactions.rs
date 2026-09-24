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

#[tokio::test]
async fn native_evse_change_and_sequence_gap_cannot_rewrite_committed_transaction() {
    let running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, _, _) = setup(&store, running.handle.clone()).await;
    add_second_evse(&mut snapshot);
    let mut frame = fixture("transaction-started");
    frame[3]["timestamp"] = json!("2026-09-01T02:00:00Z");
    let ChargerObservation::TransactionEvent(started) =
        uob_protocol_adapter::v201::decode_call(frame.to_string().as_bytes())
            .unwrap()
            .observation
    else {
        panic!("transaction")
    };
    let identity: ServiceIdentity = serde_json::from_value(json!({
        "bridge_id":snapshot.station.bridge_id,
        "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test","process_instance_id":"test"}
    })).unwrap();
    let context = |sequence| transaction16::TransactionContext {
        identity: identity.clone(),
        event_id: EventId::new(format!("evse-test-{sequence}")).unwrap(),
        sequence,
        correlation_id: None,
        target: None,
        delivery_deadline: time("2026-09-02T00:00:00Z"),
    };
    assert_eq!(
        record_transaction_event(
            &store,
            &mut snapshot,
            &started,
            context(1),
            started.occurred_at
        )
        .await
        .unwrap(),
        TransactionApplyOutcome::Applied
    );
    let durable = snapshot.clone();
    assert_eq!(
        record_transaction_event(
            &store,
            &mut snapshot,
            &started,
            context(2),
            started.occurred_at
        )
        .await
        .unwrap(),
        TransactionApplyOutcome::Duplicate
    );
    let mut changed_evse = started.clone();
    changed_evse.sequence_number = 1;
    changed_evse.native_resource = NativeProtocolReference::Ocpp201 {
        evse_id: 2,
        connector_id: Some(1),
    };
    assert!(matches!(
        record_transaction_event(
            &store,
            &mut snapshot,
            &changed_evse,
            context(3),
            started.occurred_at
        )
        .await,
        Err(ObservationCommitError::Transaction(
            TransactionApplyError::ConflictingReplay
        ))
    ));
    let mut gap = started.clone();
    gap.event = TransactionEventKind::Updated;
    gap.sequence_number = 2;
    assert!(matches!(
        record_transaction_event(&store, &mut snapshot, &gap, context(4), started.occurred_at)
            .await,
        Err(ObservationCommitError::Transaction(
            TransactionApplyError::OutOfOrder
        ))
    ));
    assert_eq!(snapshot, durable);
    assert_single_retained_event(&store, &durable).await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(store);
    let reopened = database.open();
    assert_eq!(
        reopened
            .station_snapshot(durable.station.clone())
            .await
            .unwrap(),
        Some(durable)
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

fn add_second_evse(snapshot: &mut StationSnapshot) {
    let mut second = snapshot.resources[1].clone();
    if let Some(CanonicalResource::Evse { evse_id, .. }) = &mut second.resource.resource {
        *evse_id = CanonicalEvseId::new("2").unwrap();
    }
    second.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 2,
        connector_id: Some(1),
    });
    snapshot.resources.push(second);
}

async fn assert_single_retained_event(store: &Store, snapshot: &StationSnapshot) {
    let events = store
        .read_retained_events(RetainedEventQuery {
            resource: snapshot.station.clone(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(events.events.len(), 1);
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
    let identity: ServiceIdentity = serde_json::from_value(json!({
        "bridge_id":snapshot.station.bridge_id,
        "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test","process_instance_id":"test"}
    })).unwrap();
    assert_eq!(
        record_transaction_event(
            store,
            snapshot,
            &observation,
            transaction16::TransactionContext {
                identity,
                event_id: EventId::new(format!("event-{sequence}")).unwrap(),
                sequence,
                correlation_id: None,
                target: None,
                delivery_deadline: time("2026-09-02T00:00:00Z"),
            },
            observation.occurred_at,
        )
        .await
        .unwrap(),
        TransactionApplyOutcome::Applied
    );
    let event = store
        .read_retained_events(RetainedEventQuery {
            resource: snapshot.transactions[0].resource.clone(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap()
        .events
        .pop()
        .unwrap();
    let event: EventEnvelope<TransactionSnapshot> =
        serde_json::from_value(serde_json::to_value(event).unwrap()).unwrap();
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
    assert_eq!(events.events.last().unwrap().event_id, event.event_id);
    assert_eq!(
        events.events.last().unwrap().payload,
        StationEvent::Transaction(event.payload.clone())
    );
    let station_events = store
        .read_retained_events(RetainedEventQuery {
            resource: snapshot.station.clone(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        station_events.events.last().unwrap().payload,
        StationEvent::StationSnapshot(snapshot.clone())
    );
    event
}
