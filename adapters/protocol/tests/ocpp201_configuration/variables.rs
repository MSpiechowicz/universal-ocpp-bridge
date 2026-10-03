use crate::configuration_support::reference;
use crate::{configuration_support::*, support::*};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::CommandAdmissionPort;
use uob_contracts::*;
use uob_protocol_adapter::{
    command_registry::validate_privileged_operation,
    v201::remote_control::configuration201_values::ProtectedVariableText201,
};

#[tokio::test]
async fn empty_unicode_and_all_native_statuses_are_exact_and_durable_without_values() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let names = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta"];
    let entries: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| entry(name, index))
        .collect();
    let values = [
        String::new(),
        "😀".repeat(1000),
        "WRITE-SECRET-GAMMA".into(),
        "x".into(),
        "y".into(),
        "z".into(),
    ];
    let (snapshot, _, _, commands) = configured(
        &store,
        &running,
        entries.iter().cloned().zip(values.clone()).collect(),
        None,
        Arc::new(Clock),
    )
    .await;
    learn(&mut running, &snapshot, &commands, 6, 16384, "all").await;
    let request = variables(&snapshot, "all-statuses", &entries);
    let submit = {
        let commands = commands.clone();
        tokio::spawn(async move { commands.submit(request).await.unwrap() })
    };
    let expected: Vec<Value> = names.iter().zip(&values).map(|(name, value)| json!({"component":{"name":"VendorCtrlr"},"variable":{"name":name},"attributeValue":value})).collect();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2,"all-statuses","SetVariables",{"setVariableData":expected}])
    );
    let statuses = [
        "Accepted",
        "Rejected",
        "UnknownComponent",
        "UnknownVariable",
        "NotSupportedAttributeType",
        "RebootRequired",
    ];
    let returned: Vec<Value> = names.iter().zip(statuses).rev().map(|(name, status)| json!({"component":{"name":"VENDORCTRLR"},"variable":{"name":name.to_lowercase()},"attributeStatus":status})).collect();
    running
        .peer
        .send_text(json!([3,"all-statuses",{"setVariableResult":returned}]).to_string())
        .await
        .unwrap();
    let result = submit.await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            ..
        }
    ));
    let Some(ConfigurationResult201::SetVariables { variables }) = &result.configuration_201 else {
        panic!("typed result");
    };
    assert_eq!(
        variables
            .iter()
            .map(|v| v.variable.name.as_str())
            .collect::<Vec<_>>(),
        names
    );
    assert_eq!(
        variables.iter().map(|v| v.status).collect::<Vec<_>>(),
        [
            SetVariableStatus201::Accepted,
            SetVariableStatus201::Rejected,
            SetVariableStatus201::UnknownComponent,
            SetVariableStatus201::UnknownVariable,
            SetVariableStatus201::NotSupportedAttributeType,
            SetVariableStatus201::RebootRequired
        ]
    );
    let durable = uob_application::OperationalStore::command_result_by_request_id(
        &store,
        RequestId::new("all-statuses").unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(durable, result);
    let encoded = serde_json::to_string(&durable).unwrap();
    assert!(!encoded.contains("WRITE-SECRET-GAMMA"));
    assert!(!encoded.contains("cfg201:"));
    assert!(!encoded.contains("attributeValue"));
    assert_no_secrets(&database, &["WRITE-SECRET-GAMMA", &"😀".repeat(1000)]);
    stop(running, store).await;
}

#[tokio::test]
async fn missing_extra_duplicate_or_wrong_identity_replies_are_uncertain() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let request_entries = vec![entry("Value", 1), entry("Other", 2)];
    let (snapshot, _, _, commands) = configured(
        &store,
        &running,
        request_entries
            .iter()
            .cloned()
            .map(|entry| (entry, "secret".into()))
            .collect(),
        None,
        Arc::new(Clock),
    )
    .await;
    learn(&mut running, &snapshot, &commands, 2, 4096, "correlation").await;
    let reply = json!({"component":{"name":"VendorCtrlr"},"variable":{"name":"Value"},"attributeStatus":"Accepted"});
    for (index, returned) in [
        json!([]),
        json!([reply]),
        json!([reply,reply,reply]),
        json!([reply,reply]),
        json!([reply,{"component":{"name":"VendorCtrlr"},"variable":{"name":"Foreign"},"attributeStatus":"Accepted"}]),
    ].into_iter().enumerate() {
        let id = format!("bad-reply-{index}");
        let request = variables(&snapshot, &id, &request_entries);
        let submit = { let commands = commands.clone(); tokio::spawn(async move { commands.submit(request).await.unwrap() }) };
        receive_json(&mut running.peer).await;
        running.peer.send_text(json!([3,id,{"setVariableResult":returned}]).to_string()).await.unwrap();
        let result = submit.await.unwrap();
        assert!(matches!(result.lifecycle, CommandLifecycle::TransmissionUncertain { .. }));
        assert!(result.configuration_201.is_none());
    }
    stop(running, store).await;
}

#[test]
fn unicode_caseless_and_default_actual_duplicates_and_native_character_bounds() {
    let snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    let mut a = entry("Straße", 1);
    a.component.name = "Straße".into();
    let mut b = a.clone();
    b.component.name = "STRASSE".into();
    b.variable.name = "STRASSE".into();
    b.attribute_type = Some(DeviceAttributeType201::Actual);
    b.value_reference = reference(2);
    let request = variables(&snapshot, "duplicate", &[a, b]);
    let CommandOperation::Ocpp(operation) = request.request.operation else {
        panic!("operation");
    };
    assert!(validate_privileged_operation(&snapshot.station, &operation).is_err());
    assert!(ProtectedVariableText201::new(String::new()).is_ok());
    assert!(ProtectedVariableText201::new("😀".repeat(1000)).is_ok());
    assert!(ProtectedVariableText201::new("😀".repeat(1001)).is_err());
}
