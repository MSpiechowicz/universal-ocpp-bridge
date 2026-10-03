use std::sync::Arc;

use serde_json::{Value, json};
use uob_application::{TargetDelivery, TargetMessage};
use uob_contracts::{CommandLifecycle, CommandResult, ConfigurationResult201, ContractVersion};

use crate::support::fixtures::{TestEvent, result_delivery};

pub fn variables(count: usize) -> Value {
    let statuses = [
        "Accepted",
        "Rejected",
        "UnknownComponent",
        "UnknownVariable",
        "NotSupportedAttributeType",
        "RebootRequired",
    ];
    json!({"action":"SetVariables","variables":(0..count).map(|index|json!({
        "component":{"name":"VendorCtrlr","instance":"primary"},
        "variable":{"name":format!("Setting{index}")},
        "attribute_type":"Actual","status":statuses[index % statuses.len()]
    })).collect::<Vec<_>>()})
}

pub fn reboot_required() -> Value {
    let mut evidence = variables(2);
    evidence["variables"][1]["status"] = json!("RebootRequired");
    evidence
}

pub fn network(status: &str) -> Value {
    json!({"action":"SetNetworkProfile","configuration_slot":0,
        "status":status,"staged":status == "Accepted"})
}

pub fn delivery(id: &str, evidence: Value) -> TargetDelivery<TestEvent> {
    let mut delivery = result_delivery("bridge-a", "station-a", id, id);
    let result = result_mut(&mut delivery);
    let typed: ConfigurationResult201 = serde_json::from_value(evidence).unwrap();
    result.schema_version = ContractVersion::V1_CONFIGURATION_201;
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: typed.accepted(),
        error: None,
    };
    result.configuration_201 = Some(typed);
    delivery
}

pub fn result_mut(delivery: &mut TargetDelivery<TestEvent>) -> &mut CommandResult {
    let TargetMessage::CommandResult(result) = Arc::get_mut(&mut delivery.message).unwrap() else {
        unreachable!();
    };
    result
}

pub fn assert_payload(bytes: &[u8], evidence: &Value) {
    let body: Value = serde_json::from_slice(bytes).unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../crates/contracts/schemas/v1.8/command-result.schema.json"
    ))
    .unwrap();
    jsonschema::draft202012::new(&schema)
        .unwrap()
        .validate(&body)
        .unwrap();
    let typed: CommandResult = serde_json::from_slice(bytes).unwrap();
    assert_eq!(body["schema_version"], json!({"major":1,"revision":8}));
    assert_eq!(body["configuration_201"], *evidence);
    assert_eq!(
        body["lifecycle"]["accepted"],
        typed.configuration_201.as_ref().unwrap().accepted()
    );
    assert!(typed.observed_effects.is_empty());
    let expected = match evidence["action"].as_str().unwrap() {
        "SetVariables" => vec!["action", "variables"],
        "SetNetworkProfile" => vec!["action", "configuration_slot", "staged", "status"],
        _ => unreachable!(),
    };
    assert_eq!(
        body["configuration_201"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        expected
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
    if let Some(items) = body["configuration_201"]["variables"].as_array() {
        for item in items {
            assert_eq!(
                item.as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>(),
                ["attribute_type", "component", "status", "variable"]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
            );
        }
    }
}
