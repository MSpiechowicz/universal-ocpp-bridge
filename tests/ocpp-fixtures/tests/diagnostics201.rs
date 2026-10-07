use jsonschema::Draft;
use serde_json::{Value, json};

fn schema(name: &str) -> jsonschema::Validator {
    let root = uob_ocpp_fixtures::corpus_root()
        .join("schemas/2.0.1")
        .join(format!("{name}.json"));
    let source: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(Draft::Draft6)
        .should_validate_formats(true)
        .build(&source)
        .unwrap()
}

#[test]
fn negative_cases_separate_schema_floor_from_native_semantics() {
    let cases: Value =
        serde_json::from_str(include_str!("../corpus/wire/2.0.1/log-negative-cases.json")).unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(cases.len() >= 30);
    for case in cases {
        let wire = &case["wire"];
        // CALL frames name their action; CALLRESULT cases name their response schema.
        let (name, payload) = match wire[0].as_u64() {
            Some(2) => (
                match wire[2].as_str().unwrap() {
                    "GetLog" => "GetLogRequest",
                    "LogStatusNotification" => "LogStatusNotificationRequest",
                    other => panic!("unexpected negative-case action {other}"),
                },
                &wire[3],
            ),
            Some(3) => (case["schema"].as_str().unwrap(), &wire[2]),
            _ => panic!("{} has no OCPP-J message type", case["id"]),
        };
        assert_eq!(
            schema(name).is_valid(payload),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
        // No case is accepted natively; schema-valid ones prove semantics above the schema.
        assert!(!case["native_valid"].as_bool().unwrap(), "{}", case["id"]);
    }
}

#[test]
fn status_and_reply_sets_are_exact_and_differ_from_ocpp_16() {
    let notification = schema("LogStatusNotificationRequest");
    for status in [
        "BadMessage",
        "Idle",
        "NotSupportedOperation",
        "PermissionDenied",
        "Uploaded",
        "UploadFailure",
        "Uploading",
        "AcceptedCanceled",
    ] {
        assert!(notification.is_valid(&json!({"status":status,"requestId":1})));
        // The schema leaves requestId optional; N01.FR.13 narrows it natively.
        assert!(notification.is_valid(&json!({ "status": status })));
    }
    // UploadFailed is the 1.6 DiagnosticsStatus spelling; 2.0.1 has only UploadFailure.
    assert!(!notification.is_valid(&json!({"status":"UploadFailed","requestId":1})));

    let reply = schema("GetLogResponse");
    for status in ["Accepted", "Rejected", "AcceptedCanceled"] {
        assert!(reply.is_valid(&json!({ "status": status })));
    }
    assert!(reply.is_valid(&json!({"status":"Accepted","filename":"cs-bravo.log"})));
    assert!(reply.is_valid(&json!({"status":"Rejected","statusInfo":{"reasonCode":"Busy"}})));
    assert!(!reply.is_valid(&json!({"status":"Rejected","statusInfo":{}})));
    assert!(!reply.is_valid(&json!({"status":"Accepted","fileName":"wrong-case"})));
    assert!(!reply.is_valid(&json!({"status":"Busy"})));
    assert!(!reply.is_valid(&json!({})));

    let empty = schema("LogStatusNotificationResponse");
    assert!(empty.is_valid(&json!({})));
    assert!(!empty.is_valid(&json!({"status":"Accepted"})));
}

#[test]
fn get_log_has_no_caller_destination_and_requires_the_provider_location() {
    let request = schema("GetLogRequest");
    for log_type in ["DiagnosticsLog", "SecurityLog"] {
        assert!(request.is_valid(&json!({
            "logType": log_type,
            "requestId": 1,
            "log": {"remoteLocation": "https://logs.example.test/upload/x/"}
        })));
    }
    // The native request always carries a location; the public bridge schema never does.
    assert!(!request.is_valid(&json!({"logType":"SecurityLog","requestId":1,"log":{}})));
    assert!(!request.is_valid(&json!({"logType":"SecurityLog","requestId":1})));
    assert!(!request.is_valid(&json!({
        "logType": "SecurityLog",
        "requestId": 1,
        "log": {"remoteLocation": "https://logs.example.test/"},
        "location": "https://logs.example.test/"
    })));
}

#[test]
fn every_registered_log_fixture_has_the_expected_family() {
    let registry: Value = serde_json::from_str(include_str!("../corpus/fixtures.json")).unwrap();
    let ids = registry["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|fixture| fixture["id"].as_str())
        .filter(|id| id.starts_with("wire.ocpp201.log-"))
        .count();
    // Four requests, five replies, nine notifications and the empty acknowledgement.
    assert_eq!(ids, 19);
}
