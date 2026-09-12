use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::{self, remote_control::observation::transaction_effect};
use uob_provider_adapter::LocalAuthorizationProvider;

#[tokio::test]
async fn start_reset_unlock_use_durable_authorized_wire_path_and_native_responses() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    for (name, operation, responses) in [
        (
            "remote-start",
            CommandOperation::Start {
                authorization_reference: Some(reference().await.as_str().to_owned()),
            },
            vec!["accepted", "rejected"],
        ),
        (
            "reset-soft",
            protocol("Reset", json!({"type":"Soft"})),
            vec!["accepted", "rejected"],
        ),
        (
            "reset-hard",
            protocol("Reset", json!({"type":"Hard"})),
            vec!["accepted", "rejected"],
        ),
        (
            "unlock",
            protocol("UnlockConnector", json!({"connectorId":1})),
            vec!["unlocked", "unlockfailed", "notsupported"],
        ),
    ] {
        for status in responses {
            let id = format!("{name}-{status}");
            let external = command(&snapshot, &id, operation.clone());
            let submit = {
                let commands = commands.clone();
                tokio::spawn(async move { commands.submit(external).await.unwrap() })
            };
            let mut expected = fixture(name);
            expected[1] = json!(id);
            assert_eq!(receive_json(&mut running.peer).await, expected);
            let durable = store
                .command_result_by_request_id(RequestId::new(&id).unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(durable.lifecycle, CommandLifecycle::Dispatched);
            let mut response = fixture(&format!("{name}-{status}"));
            response[1] = json!(id);
            running.peer.send_text(response.to_string()).await.unwrap();
            let result = submit.await.unwrap();
            assert!(
                matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == (status=="accepted" || status=="unlocked"))
            );
            assert!(result.observed_effects.is_empty());
            assert_eq!(
                store
                    .command_result_by_request_id(RequestId::new(&id).unwrap())
                    .await
                    .unwrap(),
                Some(result)
            );
            assert_eq!(persisted(&store).await.transactions.len(), 0);
        }
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the ordered wire/durable assertions together.
async fn transaction_notifications_commit_and_reconcile_separately_from_acceptance() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, auth, port, coordinator) = setup(&store, running.handle.clone()).await;
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
    running
        .peer
        .send_text(fixture("remote-start-accepted").to_string())
        .await
        .unwrap();
    let result = submit.await.unwrap();
    accepted(&result);
    assert!(result.observed_effects.is_empty());
    // The independent charger reports a transaction only after the command reply.
    let mut frame = fixture("start-transaction");
    frame[3]["timestamp"] = json!("2026-09-01T02:00:00Z");
    transaction(&mut running, &store, &auth, &mut snapshot, frame, 1).await;
    port.update_committed(snapshot.clone()).unwrap();
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Pending
    );
    let event = store
        .read_retained_events(RetainedEventQuery {
            resource: snapshot.resources[0].resource.clone(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap()
        .events
        .remove(0);
    let durable = store
        .command_by_request_id(start.request.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    let effect = transaction_effect(&durable, &event).unwrap();
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
        .ocpp16
        .as_mut()
        .unwrap()
        .identity_reference = Some("other".to_owned());
    assert!(transaction_effect(&durable, &unrelated).is_none());
    // Read actual persisted native ID; never parse a canonical ID as a wire integer.
    let tx = snapshot.transactions[0].transaction_id.clone();
    let stop = command(
        &snapshot,
        "remote-stop",
        CommandOperation::Stop { transaction_id: tx },
    );
    let mut denied = stop.clone();
    denied.request.request_id = RequestId::new("stop-denied").unwrap();
    let denied_submit = {
        let commands = commands.clone();
        tokio::spawn(async move { commands.submit(denied).await.unwrap() })
    };
    let sent = receive_json(&mut running.peer).await;
    assert_eq!(sent[3], fixture("remote-stop")[3]);
    let mut denied_response = fixture("remote-stop-rejected");
    denied_response[1] = json!("stop-denied");
    running
        .peer
        .send_text(denied_response.to_string())
        .await
        .unwrap();
    assert!(matches!(
        denied_submit.await.unwrap().lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            ..
        }
    ));
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Pending
    );
    let submit = {
        let commands = commands.clone();
        let stop = stop.clone();
        tokio::spawn(async move { commands.submit(stop).await.unwrap() })
    };
    assert_eq!(
        receive_json(&mut running.peer).await,
        fixture("remote-stop")
    );
    running
        .peer
        .send_text(fixture("remote-stop-accepted").to_string())
        .await
        .unwrap();
    accepted(&submit.await.unwrap());
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Pending
    );
    let mut frame = fixture("stop-transaction");
    frame[3]["transactionId"] = json!(1);
    frame[3]["timestamp"] = json!("2026-09-01T02:01:00Z");
    transaction(&mut running, &store, &auth, &mut snapshot, frame, 2).await;
    assert_eq!(
        persisted(&store).await.transactions[0].state,
        TransactionState::Ended
    );
    let event = store
        .read_retained_events(RetainedEventQuery {
            resource: snapshot.resources[0].resource.clone(),
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap()
        .events
        .remove(1);
    let durable = store
        .command_by_request_id(stop.request.request_id)
        .await
        .unwrap()
        .unwrap();
    let linked = coordinator
        .reconcile_observed_effect(
            durable.request_id.clone(),
            transaction_effect(&durable, &event).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    accepted(&linked);
    assert_eq!(linked.observed_effects.len(), 1);
    assert_eq!(
        store
            .command_result_by_request_id(durable.request_id)
            .await
            .unwrap(),
        Some(linked)
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn transaction(
    running: &mut RunningSession,
    store: &Store,
    auth: &Auth,
    snapshot: &mut StationSnapshot,
    frame: serde_json::Value,
    sequence: u64,
) {
    running.peer.send_text(frame.to_string()).await.unwrap();
    let incoming = running.outputs.incoming.receive().await.unwrap();
    let identity:ServiceIdentity=serde_json::from_value(json!({"bridge_id":snapshot.station.bridge_id,"runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test","process_instance_id":"test"}})).unwrap();
    let response = v16::complete_transaction(
        incoming.call,
        snapshot,
        &v16::TransactionServices {
            store,
            authorization: auth,
            provider: &LocalAuthorizationProvider,
            clock: &Clock,
            authorization_timeout: Duration::from_secs(1),
        },
        transaction16::TransactionContext {
            identity,
            event_id: EventId::new(format!("event-{sequence}")).unwrap(),
            sequence,
            correlation_id: None,
            target: None,
            delivery_deadline: time("2026-09-02T00:00:00Z"),
        },
    )
    .await
    .unwrap();
    incoming.responder.respond(&response[2]).unwrap();
    assert_eq!(receive_json(&mut running.peer).await, response);
}
