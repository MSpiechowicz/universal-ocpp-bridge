use super::support::*;
use std::{sync::Arc, time::Duration};
use uob_application::{
    remote_control::{RemoteControlEvidence, RemoteControlStore},
    *,
};
use uob_contracts::*;
use uob_protocol_adapter::v201::{self, remote_control::RemoteControlSession};

struct FailedEvidence;
impl RemoteControlStore for FailedEvidence {
    fn reserve_remote_start(&self, _: RequestId) -> StorageFuture<'_, i32> {
        unreachable!()
    }
    fn remote_control_evidence(
        &self,
        _: RequestId,
    ) -> StorageFuture<'_, Option<RemoteControlEvidence>> {
        unreachable!()
    }
    fn record_remote_response(
        &self,
        _: RequestId,
        _: String,
        _: Option<String>,
    ) -> StorageFuture<'_, ()> {
        Box::pin(async {
            Err(StorageError::new(
                StorageErrorCode::Unavailable,
                "injected evidence failure",
            ))
        })
    }
}

#[tokio::test]
async fn native_evidence_failure_keeps_the_dispatched_command_uncertain() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = open(&database);
    let (snapshot, _, _) = setup(&store, running.handle.clone()).await;
    let port = Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            Arc::new(Identity),
            Arc::new(Clock),
            Arc::new(FailedEvidence),
        )
        .unwrap(),
    );
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port,
        Arc::new(Clock),
    ));
    let external = command(&snapshot, "evidence-failure", 1, "Inoperative");
    let result = exchange(
        &mut running,
        scoped(
            coordinator.clone(),
            &snapshot,
            vec![AccessPermission::PrivilegedControl],
        ),
        external.clone(),
        "Scheduled",
    )
    .await;
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert!(result.observed_effects.is_empty());
    assert_eq!(persisted(&store).await, snapshot);
    assert!(
        store
            .remote_control_evidence(external.request.request_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(coordinator.submit(external).await.unwrap(), result);
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
async fn connector_unavailability_blocks_evse_start_until_observed_operative() {
    use crate::remote_support as remote;
    let mut running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = remote::setup(&store, running.handle.clone()).await;
    for (index, state) in ["Unavailable", "Faulted", "Available"]
        .into_iter()
        .enumerate()
    {
        for connector in [1, 2] {
            let frame = serde_json::json!([2,"connector","StatusNotification",{"evseId":1,"connectorId":connector,"connectorStatus":state,"timestamp":"2026-09-01T02:00:00Z"}]);
            v201::registration_call(
                frame.to_string().as_bytes(),
                &store,
                &mut snapshot,
                registration::RegistrationDecision::Accepted,
                60,
                Clock.now(),
            )
            .await
            .unwrap();
        }
        port.update_committed(snapshot.clone()).unwrap();
        let external = remote::command(
            &snapshot,
            &format!("start-{index}"),
            CommandOperation::Start {
                authorization_reference: Some(remote::reference().await.as_str().to_owned()),
            },
        );
        if state == "Available" {
            let coordinator = coordinator.clone();
            let submit = tokio::spawn(async move { coordinator.submit(external).await.unwrap() });
            let call = receive_json(&mut running.peer).await;
            assert_eq!(call[2], "RequestStartTransaction");
            running
                .peer
                .send_text(serde_json::json!([3,call[1],{"status":"Accepted"}]).to_string())
                .await
                .unwrap();
            remote::accepted(&submit.await.unwrap());
        } else {
            assert!(matches!(
                coordinator.submit(external).await.unwrap().lifecycle,
                CommandLifecycle::Rejected {
                    error: CommandError {
                        code: CommandErrorCode::PolicyRejected,
                        ..
                    }
                }
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
                    .await
                    .is_err()
            );
        }
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
