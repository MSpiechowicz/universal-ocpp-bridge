use crate::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
async fn caseless_identity_preserves_original_names_and_empty_value_without_learning_secret_limits()
{
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    let payload =
        json!({"getVariableData":[{"component":{"name":"Straße"},"variable":{"name":"Token"}}]});
    let mut external = command(
        &snapshot,
        "caseless-empty",
        protocol("GetVariables", payload.clone()),
    );
    external.request.resource = snapshot.station.clone();
    let mut submitted = tokio::spawn(async move { commands.submit(external).await.unwrap() });
    let wire = tokio::select! {
        wire = receive_json(&mut running.peer) => wire,
        result = &mut submitted => panic!("native submission completed before CALL: {result:?}"),
    };
    assert_eq!(wire, json!([2, "caseless-empty", "GetVariables", payload]));
    running.peer.send_text(json!([3,"caseless-empty",{"getVariableResult":[{"attributeStatus":"Accepted","component":{"name":"STRASSE"},"variable":{"name":"TOKEN"},"attributeValue":""}]}]).to_string()).await.unwrap();
    let result = submitted.await.unwrap();
    accepted(&result);
    let evidence = result.device_model_201.unwrap();
    let value = &evidence.variables[0];
    assert_eq!(value.component.name, "STRASSE");
    assert_eq!(
        value.value,
        DeviceValue201 {
            present: true,
            redacted: true,
            empty: true,
            value: None
        }
    );
    assert_eq!(evidence.report, DeviceReportState201::NotExpected);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn response_only_identities_and_wrong_instances_are_uncertain_without_partial_evidence() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    for (id, name) in [("extra-identity", "Other"), ("wrong-instance", "Requested")] {
        let mut external = command(
            &snapshot,
            id,
            protocol(
                "GetVariables",
                json!({"getVariableData":[{"component":{"name":"Ctrlr","instance":"A"},"variable":{"name":"Requested"}}]}),
            ),
        );
        external.request.resource = snapshot.station.clone();
        let port = commands.clone();
        let submitted = tokio::spawn(async move { port.submit(external).await.unwrap() });
        receive_json(&mut running.peer).await;
        running.peer.send_text(json!([3,id,{"getVariableResult":[{"attributeStatus":"UnknownVariable","component":{"name":"Ctrlr","instance":"B"},"variable":{"name":name}}]}]).to_string()).await.unwrap();
        let result = submitted.await.unwrap();
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
        assert!(result.device_model_201.unwrap().variables.is_empty());
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn unknown_multi_item_limit_and_custom_data_are_rejected_before_any_native_dispatch() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    );
    for (id, payload) in [
        (
            "unknown-limit",
            json!({"getVariableData":[{"component":{"name":"Ctrlr"},"variable":{"name":"A"}},{"component":{"name":"Ctrlr"},"variable":{"name":"B"}}]}),
        ),
        (
            "custom-data",
            json!({"getVariableData":[{"component":{"name":"Ctrlr","customData":{"vendorId":"PRIVATE"}},"variable":{"name":"A"}}]}),
        ),
    ] {
        let mut external = command(&snapshot, id, protocol("GetVariables", payload));
        external.request.resource = snapshot.station.clone();
        let result = commands.submit(external).await.unwrap();
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::Rejected { .. }
        ));
        assert!(
            result
                .device_model_201
                .is_none_or(|value| value.variables.is_empty())
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
