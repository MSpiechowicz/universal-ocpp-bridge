use serde_json::{Value, json};
use uob_application::ChargerObservation;
use uob_contracts::{LogUploadStatus16, TriggerMessageClass};
use uob_protocol_adapter::{DecodeErrorKind, v16::decode_call};

fn frame(action: &str, payload: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!([2, "log-1", action, payload])).unwrap()
}

const LOG_STATUSES: [&str; 7] = [
    "BadMessage",
    "Idle",
    "NotSupportedOperation",
    "PermissionDenied",
    "Uploaded",
    "UploadFailure",
    "Uploading",
];

#[test]
fn every_whitepaper_log_status_decodes_with_its_request_identity() {
    for status in LOG_STATUSES {
        for request_id in [i32::MIN, 0, i32::MAX] {
            let decoded = decode_call(&frame(
                "LogStatusNotification",
                &json!({"status":status,"requestId":request_id}),
            ))
            .unwrap();
            assert_eq!(decoded.action.as_str(), "LogStatusNotification");
            assert_eq!(
                decoded.observation,
                ChargerObservation::LogStatus16 {
                    status: serde_json::from_value::<LogUploadStatus16>(json!(status)).unwrap(),
                    request_id: Some(request_id),
                }
            );
        }
    }
    // N01.FR.12: only a triggered Idle may omit the upload identity.
    let idle = decode_call(&frame("LogStatusNotification", &json!({"status":"Idle"}))).unwrap();
    assert_eq!(
        idle.observation,
        ChargerObservation::LogStatus16 {
            status: LogUploadStatus16::Idle,
            request_id: None,
        }
    );
    // The legacy notification keeps its trigger-status observation.
    let legacy = decode_call(&frame(
        "DiagnosticsStatusNotification",
        &json!({"status":"Uploaded"}),
    ))
    .unwrap();
    assert_eq!(
        legacy.observation,
        ChargerObservation::TriggerStatus {
            class: TriggerMessageClass::DiagnosticsStatusNotification,
            status: "Uploaded".to_owned(),
        }
    );
}

#[test]
fn malformed_log_notifications_are_property_violations() {
    for payload in [
        json!({"status":"Uploaded"}),
        json!({"status":"Uploading","requestId":null}),
        json!({"status":"Uploaded","requestId":2_147_483_648_i64}),
        json!({"status":"Uploaded","requestId":"1"}),
        json!({"status":"UploadFailed","requestId":1}),
        json!({"status":"AcceptedCanceled","requestId":1}),
        json!({"status":"uploaded","requestId":1}),
        json!({"status":"Uploaded","requestId":1,"fileName":"x"}),
        json!({}),
    ] {
        let error = decode_call(&frame("LogStatusNotification", &payload)).unwrap_err();
        assert_eq!(error.kind(), DecodeErrorKind::InvalidPayload, "{payload}");
        assert_eq!(
            error.call_error().code.as_str(),
            "PropertyConstraintViolation"
        );
    }
}

fn corpus() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/ocpp-fixtures/corpus")
}

#[test]
fn every_independent_inbound_log_fixture_decodes_and_negative_cases_do_not() {
    let registry: Value =
        serde_json::from_slice(&std::fs::read(corpus().join("fixtures.json")).unwrap()).unwrap();
    let mut decoded = 0;
    for fixture in registry["fixtures"].as_array().unwrap() {
        let action = fixture["action"].as_str().unwrap();
        if fixture["message_type"] != 2
            || fixture["protocol_version"] != "1.6"
            || !matches!(
                action,
                "DiagnosticsStatusNotification" | "LogStatusNotification"
            )
        {
            continue;
        }
        let bytes = std::fs::read(corpus().join(fixture["wire"].as_str().unwrap())).unwrap();
        let call = decode_call(&bytes).unwrap_or_else(|_| panic!("{}", fixture["id"]));
        assert_eq!(call.action.as_str(), action);
        decoded += 1;
    }
    // Four legacy and eight log notifications, plus the earlier trigger-result fixture.
    assert!(decoded >= 13, "{decoded}");
    let cases: Value = serde_json::from_slice(
        &std::fs::read(corpus().join("wire/1.6/diagnostics-negative-cases.json")).unwrap(),
    )
    .unwrap();
    let mut refused = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][0] != 2
            || !case["wire"][2]
                .as_str()
                .unwrap()
                .ends_with("StatusNotification")
        {
            continue;
        }
        let bytes = serde_json::to_vec(&case["wire"]).unwrap();
        assert!(decode_call(&bytes).is_err(), "{}", case["id"]);
        refused += 1;
    }
    assert_eq!(refused, 8);
}
