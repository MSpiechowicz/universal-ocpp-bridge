use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep each failure mode and restart sequence together.
async fn delayed_malformed_and_disconnected_results_remain_uncertain_after_restart() {
    for mode in ["timeout", "malformed", "disconnect", "callerror"] {
        let mut running = session("ocpp2.0.1", Duration::from_millis(120)).await;
        let database = Database::new();
        let store = database.open();
        let (snapshot, _, _, coordinator) = setup(&store, running.handle.clone()).await;
        let commands = Arc::new(scoped(
            coordinator.clone(),
            &snapshot,
            vec![
                AccessPermission::PrivilegedControl,
                AccessPermission::Control,
            ],
        ));
        let operation = if matches!(mode, "timeout" | "disconnect") {
            CommandOperation::Start {
                authorization_reference: Some(reference().await.as_str().to_owned()),
            }
        } else {
            protocol("Reset", json!({"type":"OnIdle"}))
        };
        let external = command(&snapshot, mode, operation);
        let submit = {
            let commands = commands.clone();
            let external = external.clone();
            tokio::spawn(async move { commands.submit(external).await.unwrap() })
        };
        assert_eq!(receive_json(&mut running.peer).await[1], mode);
        match mode {
            "malformed" => running
                .peer
                .send_text(
                    json!([3,mode,{"status":"Accepted","secret":"must not be logged"}]).to_string(),
                )
                .await
                .unwrap(),
            "disconnect" => running.peer.disconnect().await.unwrap(),
            "callerror" => running
                .peer
                .send_text(json!([4, mode, "NotSupported", "private secret", {}]).to_string())
                .await
                .unwrap(),
            _ => {}
        }
        let result = submit.await.unwrap();
        if mode == "callerror" {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::ProtocolResponse {
                    accepted: false,
                    ..
                }
            ));
        } else {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::TransmissionUncertain { .. }
            ));
        }
        assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
        if mode == "timeout" {
            running
                .peer
                .send_text(json!([3,mode,{"status":"Accepted"}]).to_string())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), running.outputs.diagnostics.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                store
                    .command_result_by_request_id(external.request.request_id.clone())
                    .await
                    .unwrap(),
                Some(result.clone())
            );
        }
        if mode == "disconnect" {
            running.task.wait().await.unwrap();
        } else {
            running.task.shutdown(Duration::from_secs(1)).await.unwrap();
        }
        running.server.abort();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
        let store = database.open();
        let mut next = session("ocpp2.0.1", Duration::from_secs(1)).await;
        let (_, _, _, coordinator) = setup(&store, next.handle.clone()).await;
        let recovered = coordinator
            .recover_unresolved(PageLimit::new(100).unwrap())
            .await
            .unwrap();
        assert_eq!(recovered.commands.len(), usize::from(mode != "callerror"));
        assert_eq!(coordinator.submit(external).await.unwrap(), result);
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

#[tokio::test]
async fn crash_after_dispatch_is_recovered_without_sending_a_second_reset() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup(&store, running.handle.clone()).await;
    let external = command(
        &snapshot,
        "crash",
        protocol("Reset", json!({"type":"Immediate"})),
    );
    let submit = {
        let coordinator = coordinator.clone();
        let external = external.clone();
        tokio::spawn(async move { coordinator.submit(external).await.unwrap() })
    };
    receive_json(&mut running.peer).await;
    submit.abort();
    let _ = submit.await;
    assert_eq!(
        store
            .command_result_by_request_id(external.request.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        CommandLifecycle::Dispatched
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let store = database.open();
    let mut next = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let (_, _, _, coordinator) = setup(&store, next.handle.clone()).await;
    let recovered = coordinator
        .recover_unresolved(PageLimit::new(100).unwrap())
        .await
        .unwrap();
    assert_eq!(recovered.commands.len(), 1);
    assert!(matches!(
        recovered.commands[0].result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert_eq!(
        coordinator.submit(external).await.unwrap(),
        recovered.commands[0].result
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(40), next.peer.receive())
            .await
            .is_err()
    );
    next.task.shutdown(Duration::from_secs(1)).await.unwrap();
    next.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
