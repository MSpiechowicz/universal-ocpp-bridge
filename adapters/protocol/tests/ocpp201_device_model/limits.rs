use crate::support::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
async fn explicit_safe_limits_unlock_permuted_mixed_results_but_duplicate_response_is_uncertain() {
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
    learn_request_limits(&mut running, &snapshot, &commands).await;
    let component = json!({"name":"VendorCtrlr","instance":"unit-A"});
    let entries: Vec<Value> = ["Alpha", "Beta", "Gamma"]
        .into_iter()
        .map(|name| json!({"component":component,"variable":{"name":name}}))
        .collect();
    for (id, duplicate) in [("mixed-permuted", false), ("duplicate-native", true)] {
        let mut external = command(
            &snapshot,
            id,
            protocol("GetVariables", json!({"getVariableData":entries})),
        );
        external.request.resource = snapshot.station.clone();
        let port = commands.clone();
        let submitted = tokio::spawn(async move { port.submit(external).await.unwrap() });
        receive_json(&mut running.peer).await;
        let mut returned = vec![
            json!({"component":component,"variable":{"name":"Gamma"},"attributeStatus":"Accepted","attributeValue":""}),
            json!({"component":component,"variable":{"name":"Beta"},"attributeStatus":"UnknownVariable"}),
            json!({"component":component,"variable":{"name":"Alpha"},"attributeStatus":"Accepted","attributeValue":"SECRET-MIXED"}),
        ];
        if duplicate {
            returned[2] = returned[0].clone();
        }
        running
            .peer
            .send_text(json!([3,id,{"getVariableResult":returned}]).to_string())
            .await
            .unwrap();
        let result = submitted.await.unwrap();
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("SECRET-MIXED")
        );
        if duplicate {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::TransmissionUncertain { .. }
            ));
            assert!(result.device_model_201.unwrap().variables.is_empty());
        } else {
            accepted(&result);
            let evidence = result.device_model_201.unwrap();
            assert_eq!(
                evidence
                    .variables
                    .iter()
                    .map(|item| item.variable.name.as_str())
                    .collect::<Vec<_>>(),
                ["Gamma", "Beta", "Alpha"]
            );
            assert_eq!(
                evidence.variables[1].status,
                DeviceVariableStatus201::UnknownVariable
            );
            assert!(!evidence.variables[1].value.present);
            assert!(evidence.variables[0].value.empty);
            assert!(evidence.variables[2].value.redacted);
        }
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn learn_request_limits(
    running: &mut RunningSession,
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
) {
    for (index, (name, value)) in [("ItemsPerMessage", "4"), ("BytesPerMessage", "4096")]
        .into_iter()
        .enumerate()
    {
        let id = format!("learn-{index}");
        let component = json!({"name":"DeviceDataCtrlr"});
        let variable = json!({"name":name,"instance":"GetVariables"});
        let mut external = command(
            snapshot,
            &id,
            protocol(
                "GetVariables",
                json!({"getVariableData":[{"component":component,"variable":variable}]}),
            ),
        );
        external.request.resource = snapshot.station.clone();
        let port = Arc::clone(commands);
        let submitted = tokio::spawn(async move { port.submit(external).await.unwrap() });
        receive_json(&mut running.peer).await;
        let variable = json!({"name":name.replace('s', "ſ"),"instance":"GetVariableſ"});
        running.peer.send_text(json!([3,id,{"getVariableResult":[{"component":component,"variable":variable,"attributeStatus":"Accepted","attributeValue":value}]}]).to_string()).await.unwrap();
        let result = submitted.await.unwrap();
        assert_eq!(
            result.device_model_201.unwrap().variables[0]
                .value
                .value
                .as_deref(),
            Some(value)
        );
    }
}
