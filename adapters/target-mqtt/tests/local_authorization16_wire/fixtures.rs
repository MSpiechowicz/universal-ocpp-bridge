use crate::support::fixtures::{TestEvent, result_delivery};
use serde_json::{Value, json};
use std::sync::Arc;
use uob_application::{TargetDelivery, TargetMessage};
use uob_contracts::{CommandLifecycle, CommandResult, ContractVersion, LocalAuthorizationResult16};

pub fn cases() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("ordinary", None),
        (
            "query-empty",
            Some(json!({"action":"GetLocalListVersion","list_version":0})),
        ),
        (
            "query-unsupported",
            Some(json!({"action":"GetLocalListVersion","list_version":-1})),
        ),
        (
            "query-negative",
            Some(json!({"action":"GetLocalListVersion","list_version":-2})),
        ),
        (
            "full-accepted",
            Some(
                json!({"action":"SendLocalList","list_version":-2,"update_type":"Full","status":"Accepted"}),
            ),
        ),
        (
            "diff-failed",
            Some(
                json!({"action":"SendLocalList","list_version":5,"update_type":"Differential","status":"Failed"}),
            ),
        ),
        (
            "diff-unsupported",
            Some(
                json!({"action":"SendLocalList","list_version":6,"update_type":"Differential","status":"NotSupported"}),
            ),
        ),
        (
            "diff-mismatch",
            Some(
                json!({"action":"SendLocalList","list_version":7,"update_type":"Differential","status":"VersionMismatch"}),
            ),
        ),
        (
            "cache-accepted",
            Some(json!({"action":"ClearCache","status":"Accepted"})),
        ),
        (
            "cache-rejected",
            Some(json!({"action":"ClearCache","status":"Rejected"})),
        ),
    ]
}
pub fn delivery(id: &str, evidence: Option<Value>) -> TargetDelivery<TestEvent> {
    let mut delivery = result_delivery("bridge-a", "station-a", id, id);
    let result = result_mut(&mut delivery);
    result.schema_version = ContractVersion::V1_LOCAL_AUTHORIZATION_16;
    result.local_authorization_16 =
        evidence.map(|value| serde_json::from_value::<LocalAuthorizationResult16>(value).unwrap());
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: result
            .local_authorization_16
            .as_ref()
            .is_none_or(LocalAuthorizationResult16::accepted),
        error: None,
    };
    delivery
}
pub fn result_mut(delivery: &mut TargetDelivery<TestEvent>) -> &mut CommandResult {
    let TargetMessage::CommandResult(result) = Arc::get_mut(&mut delivery.message).unwrap() else {
        unreachable!()
    };
    result
}
pub fn assert_payload(bytes: &[u8], evidence: Option<&Value>) {
    let body: Value = serde_json::from_slice(bytes).unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../crates/contracts/schemas/v1.9/command-result.schema.json"
    ))
    .unwrap();
    jsonschema::draft202012::new(&schema)
        .unwrap()
        .validate(&body)
        .unwrap();
    let typed: CommandResult = serde_json::from_slice(bytes).unwrap();
    assert_eq!(body["schema_version"], json!({"major":1,"revision":9}));
    if let Some(evidence) = evidence {
        assert_eq!(body["local_authorization_16"], *evidence);
        assert_eq!(
            body["lifecycle"]["accepted"],
            typed.local_authorization_16.as_ref().unwrap().accepted()
        );
        let keys = evidence
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let expected: &[&str] = match evidence["action"].as_str().unwrap() {
            "GetLocalListVersion" => &["action", "list_version"],
            "SendLocalList" => &["action", "list_version", "update_type", "status"],
            "ClearCache" => &["action", "status"],
            _ => unreachable!(),
        };
        assert_eq!(keys, expected.iter().copied().collect());
    } else {
        assert!(body.get("local_authorization_16").is_none());
    }
    let text = std::str::from_utf8(bytes).unwrap();
    for forbidden in [
        "idTag",
        "parentIdTag",
        "localAuthorizationList",
        "updateReference",
        "list16:",
        "PRIVATE-LIST-111",
        "PRIVATE-PARENT-111",
    ] {
        assert!(!text.contains(forbidden));
    }
}
