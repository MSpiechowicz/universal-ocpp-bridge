use jsonschema::Draft;
use serde_json::{Value, json};

fn schema(name: &str) -> jsonschema::Validator {
    let (directory, draft) = if name.starts_with("GetLog") || name.starts_with("LogStatus") {
        ("schemas/1.6-security", Draft::Draft6)
    } else {
        ("schemas/1.6", Draft::Draft4)
    };
    let root = uob_ocpp_fixtures::corpus_root()
        .join(directory)
        .join(format!("{name}.json"));
    let source: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(draft)
        .should_validate_formats(true)
        .build(&source)
        .unwrap()
}

#[test]
fn negative_cases_separate_schema_floor_from_native_semantics() {
    let cases: Value = serde_json::from_str(include_str!(
        "../corpus/wire/1.6/diagnostics-negative-cases.json"
    ))
    .unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(cases.len() >= 24);
    for case in cases {
        let wire = &case["wire"];
        // CALL frames name their action; CALLRESULT cases name their response schema.
        let (name, payload) = match wire[0].as_u64() {
            Some(2) => (wire[2].as_str().unwrap(), &wire[3]),
            Some(3) => (case["schema"].as_str().unwrap(), &wire[2]),
            _ => panic!("{} has no OCPP-J message type", case["id"]),
        };
        assert_eq!(
            schema(name).is_valid(payload),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
        // No case is accepted natively; schema-valid ones prove the bridge adds semantics.
        assert!(!case["native_valid"].as_bool().unwrap(), "{}", case["id"]);
    }
}

#[test]
fn status_and_reply_sets_are_exact_per_message_family() {
    let legacy = schema("DiagnosticsStatusNotification");
    let log = schema("LogStatusNotification");
    for status in ["Idle", "Uploaded", "UploadFailed", "Uploading"] {
        assert!(legacy.is_valid(&json!({"status":status})));
    }
    for status in [
        "BadMessage",
        "Idle",
        "NotSupportedOperation",
        "PermissionDenied",
        "Uploaded",
        "UploadFailure",
        "Uploading",
    ] {
        assert!(log.is_valid(&json!({"status":status,"requestId":1})));
    }
    // The two families name their failure differently and share no other failure value.
    for status in [
        "UploadFailure",
        "BadMessage",
        "NotSupportedOperation",
        "PermissionDenied",
    ] {
        assert!(!legacy.is_valid(&json!({"status":status})));
    }
    assert!(!log.is_valid(&json!({"status":"UploadFailed","requestId":1})));
    // Edition 4 made requestId optional on the wire (N01.FR.12 decides when it may be absent).
    assert!(log.is_valid(&json!({"status":"Uploading"})));

    let reply = schema("GetLogResponse");
    for status in ["Accepted", "Rejected", "AcceptedCanceled"] {
        assert!(reply.is_valid(&json!({"status":status})));
    }
    assert!(!reply.is_valid(&json!({"status":"Busy"})));
    assert!(!reply.is_valid(&json!({})));
    // The legacy reply has no status; only the optional file name (6.26).
    let legacy_reply = schema("GetDiagnosticsResponse");
    assert!(legacy_reply.is_valid(&json!({})));
    assert!(legacy_reply.is_valid(&json!({"fileName":"diagnostics.tar.gz"})));
    assert!(!legacy_reply.is_valid(&json!({"status":"Accepted"})));
    for log_type in ["DiagnosticsLog", "SecurityLog"] {
        assert!(schema("GetLog").is_valid(&json!({
            "logType": log_type,
            "requestId": 1,
            "log": {"remoteLocation": "https://uploads.demo.invalid/u/"}
        })));
    }
}

#[test]
fn every_wire_fixture_validates_with_formats() {
    let manifest: Value = serde_json::from_str(include_str!("../corpus/fixtures.json")).unwrap();
    let root = uob_ocpp_fixtures::corpus_root();
    let mut checked = 0;
    for fixture in manifest["fixtures"].as_array().unwrap() {
        let id = fixture["id"].as_str().unwrap();
        if !(id.starts_with("wire.ocpp16.diagnostics-") || id.starts_with("wire.ocpp16.log-")) {
            continue;
        }
        let path = fixture["schema"].as_str().unwrap();
        let name = path.rsplit('/').next().unwrap().trim_end_matches(".json");
        let frame: Value = serde_json::from_slice(
            &std::fs::read(root.join(fixture["wire"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        let payload = frame.as_array().unwrap().last().unwrap();
        assert!(schema(name).is_valid(payload), "{id}");
        checked += 1;
    }
    assert_eq!(checked, 26);
}
