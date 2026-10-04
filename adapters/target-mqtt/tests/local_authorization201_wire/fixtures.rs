use crate::support::fixtures::{TestEvent, result_delivery};
use serde_json::{Value, json};
use std::sync::Arc;
use uob_application::{TargetDelivery, TargetMessage};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, LocalAuthorizationResult201,
};

pub fn cases() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("ordinary", None),
        (
            "query-empty",
            Some(json!({"action":"GetLocalListVersion","version_number":0})),
        ),
        (
            "query-zero",
            Some(json!({"action":"GetLocalListVersion","version_number":0})),
        ),
        (
            "query-installed",
            Some(json!({"action":"GetLocalListVersion","version_number":1})),
        ),
        (
            "full-accepted",
            Some(
                json!({"action":"SendLocalList","version_number":1,"update_type":"Full","status":"Accepted"}),
            ),
        ),
        (
            "diff-failed",
            Some(
                json!({"action":"SendLocalList","version_number":5,"update_type":"Differential","status":"Failed"}),
            ),
        ),
        (
            "diff-failed-again",
            Some(
                json!({"action":"SendLocalList","version_number":6,"update_type":"Differential","status":"Failed"}),
            ),
        ),
        (
            "diff-mismatch",
            Some(
                json!({"action":"SendLocalList","version_number":7,"update_type":"Differential","status":"VersionMismatch"}),
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
    result.schema_version = ContractVersion::V1_LOCAL_AUTHORIZATION_201;
    result.local_authorization_201 =
        evidence.map(|value| serde_json::from_value::<LocalAuthorizationResult201>(value).unwrap());
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: result
            .local_authorization_201
            .as_ref()
            .is_none_or(LocalAuthorizationResult201::accepted),
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
        "../../../../crates/contracts/schemas/v1.10/command-result.schema.json"
    ))
    .unwrap();
    jsonschema::draft202012::new(&schema)
        .unwrap()
        .validate(&body)
        .unwrap();
    let typed: CommandResult = serde_json::from_slice(bytes).unwrap();
    assert_eq!(body["schema_version"], json!({"major":1,"revision":10}));
    if let Some(evidence) = evidence {
        assert_eq!(body["local_authorization_201"], *evidence);
        assert_eq!(
            body["lifecycle"]["accepted"],
            typed.local_authorization_201.as_ref().unwrap().accepted()
        );
        let keys = evidence
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let expected: &[&str] = match evidence["action"].as_str().unwrap() {
            "GetLocalListVersion" => &["action", "version_number"],
            "SendLocalList" => &["action", "version_number", "update_type", "status"],
            "ClearCache" => &["action", "status"],
            _ => unreachable!(),
        };
        assert_eq!(keys, expected.iter().copied().collect());
    } else {
        assert!(body.get("local_authorization_201").is_none());
    }
    let text = std::str::from_utf8(bytes).unwrap();
    for forbidden in [
        "idTag",
        "parentIdTag",
        "localAuthorizationList",
        "updateReference",
        "list201:",
        "PRIVATE-LIST-111",
        "PRIVATE-PARENT-111",
    ] {
        assert!(!text.contains(forbidden));
    }
}
