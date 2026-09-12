use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::{remote_control::RemoteControlStore, *};
use uob_contracts::*;
use uob_protocol_adapter::v201::{
    availability::observed_effect, remote_control::RemoteControlSession,
};

#[tokio::test]
async fn restart_retains_scheduled_intent_and_reconciles_without_replay() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = open(&database);
    let (snapshot, _, coordinator) = setup(&store, running.handle.clone()).await;
    let external = command(&snapshot, "restart", 1, "Inoperative");
    let result = exchange(
        &mut running,
        scoped(
            coordinator,
            &snapshot,
            vec![AccessPermission::PrivilegedControl],
        ),
        external.clone(),
        "Scheduled",
    )
    .await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let store = open(&database);
    let mut snapshot = persisted(&store).await;
    assert_eq!(
        snapshot.resources[0].availability,
        AvailabilityState::Available
    );
    assert_eq!(
        store
            .remote_control_evidence(external.request.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .response_status
            .as_deref(),
        Some("Scheduled")
    );
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let port = Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            Arc::new(Identity),
            Arc::new(Clock),
            Arc::new(store.clone()),
        )
        .unwrap(),
    );
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port.clone(),
        Arc::new(Clock),
    ));
    assert_eq!(coordinator.submit(external.clone()).await.unwrap(), result);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    status(&mut running, &store, &mut snapshot, 1, "Unavailable", 1).await;
    port.update_committed(snapshot.clone()).unwrap();
    let durable = store
        .command_by_request_id(external.request.request_id.clone())
        .await
        .unwrap()
        .unwrap();
    let effect = observed_effect(&durable, &event(&store, &snapshot).await).unwrap();
    let result = coordinator
        .reconcile_observed_effect(durable.request_id, effect)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.observed_effects.len(), 1);
    assert_eq!(coordinator.submit(external).await.unwrap(), result);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let store = open(&database);
    assert_eq!(
        persisted(&store).await.resources[0].availability,
        AvailabilityState::Unavailable
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn missing_malformed_late_and_callerror_responses_never_invent_availability() {
    for mode in ["timeout", "malformed", "callerror", "disconnect", "crash"] {
        let mut running = session("ocpp2.0.1", Duration::from_millis(100)).await;
        let database = Database::new();
        let store = open(&database);
        let (snapshot, _, coordinator) = setup(&store, running.handle.clone()).await;
        let external = command(&snapshot, mode, 1, "Inoperative");
        let submit = {
            let coordinator = coordinator.clone();
            let external = external.clone();
            tokio::spawn(async move { coordinator.submit(external).await.unwrap() })
        };
        receive_json(&mut running.peer).await;
        match mode {
            "malformed" => running
                .peer
                .send_text(json!([3,mode,{"status":"Bogus"}]).to_string())
                .await
                .unwrap(),
            "callerror" => running
                .peer
                .send_text(json!([4, mode, "NotSupported", "", {}]).to_string())
                .await
                .unwrap(),
            "disconnect" => {
                running.peer.disconnect().await.unwrap();
            }
            "crash" => submit.abort(),
            _ => {}
        }
        if mode == "crash" {
            let _ = submit.await;
        } else {
            let result = submit.await.unwrap();
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::TransmissionUncertain { .. }
                    | CommandLifecycle::ProtocolResponse {
                        accepted: false,
                        ..
                    }
            ));
        }
        if mode == "timeout" {
            running
                .peer
                .send_text(json!([3,mode,{"status":"Accepted"}]).to_string())
                .await
                .unwrap();
        }
        assert_eq!(persisted(&store).await, snapshot);
        running.task.shutdown(Duration::from_secs(1)).await.unwrap();
        running.server.abort();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
        let store = open(&database);
        let mut next = session("ocpp2.0.1", Duration::from_secs(1)).await;
        let (_, _, coordinator) = setup(&store, next.handle.clone()).await;
        let recovered = coordinator
            .recover_unresolved(PageLimit::new(100).unwrap())
            .await
            .unwrap();
        assert_eq!(recovered.commands.len(), usize::from(mode != "callerror"));
        let result = coordinator.submit(external).await.unwrap();
        assert!(result.observed_effects.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(40), next.peer.receive())
                .await
                .is_err()
        );
        next.task.shutdown(Duration::from_secs(1)).await.unwrap();
        next.server.abort();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}
