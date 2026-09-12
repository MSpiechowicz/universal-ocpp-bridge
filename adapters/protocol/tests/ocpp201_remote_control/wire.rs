use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::remote_control::RemoteControlStore;
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
async fn start_reset_unlock_use_durable_authorized_wire_path_and_native_responses() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
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
    let mut next_remote_id = 0;
    for (name, operation, responses) in [
        (
            "remote-start",
            CommandOperation::Start {
                authorization_reference: Some(reference().await.as_str().to_owned()),
            },
            vec!["accepted", "rejected"],
        ),
        (
            "reset-onidle",
            protocol("Reset", json!({"type":"OnIdle"})),
            vec!["accepted", "scheduled", "rejected"],
        ),
        (
            "reset-immediate",
            protocol("Reset", json!({"type":"Immediate"})),
            vec!["accepted", "rejected"],
        ),
        (
            "unlock",
            protocol("UnlockConnector", json!({"evseId":1,"connectorId":1})),
            vec![
                "unlocked",
                "unlockfailed",
                "ongoingauthorizedtransaction",
                "unknownconnector",
            ],
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
            if name == "remote-start" {
                next_remote_id += 1;
                expected[3]["remoteStartId"] = json!(next_remote_id);
            }
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
                matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == (status=="accepted" || status=="unlocked" || status=="scheduled"))
            );
            assert!(result.observed_effects.is_empty());
            let evidence = store
                .remote_control_evidence(RequestId::new(&id).unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                evidence.response_status.as_deref(),
                response[2]["status"].as_str()
            );
            if name == "remote-start" {
                assert_eq!(evidence.remote_start_id, Some(next_remote_id));
            }
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
