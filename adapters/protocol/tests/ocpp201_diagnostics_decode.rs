use serde_json::{Value, json};
use uob_application::ChargerObservation;
use uob_contracts::LogUploadStatus201;
use uob_protocol_adapter::{DecodeErrorKind, v201::decode_call};

fn frame(payload: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!([2, "log-1", "LogStatusNotification", payload])).unwrap()
}

const STATUSES: [&str; 8] = [
    "BadMessage",
    "Idle",
    "NotSupportedOperation",
    "PermissionDenied",
    "Uploaded",
    "UploadFailure",
    "Uploading",
    "AcceptedCanceled",
];

#[test]
fn every_log_status_decodes_with_its_request_identity() {
    for status in STATUSES {
        for request_id in [i32::MIN, 0, i32::MAX] {
            let decoded = decode_call(&frame(
                &json!({"status":status,"requestId":request_id,"customData":{"vendorId":"v"}}),
            ))
            .unwrap();
            assert_eq!(decoded.action.as_str(), "LogStatusNotification");
            assert_eq!(
                decoded.observation,
                ChargerObservation::LogStatus201 {
                    status: serde_json::from_value::<LogUploadStatus201>(json!(status)).unwrap(),
                    request_id: Some(request_id),
                }
            );
        }
    }
    // N01.FR.13: only a triggered Idle with no upload ongoing may omit the upload identity.
    let idle = decode_call(&frame(&json!({"status":"Idle"}))).unwrap();
    assert_eq!(
        idle.observation,
        ChargerObservation::LogStatus201 {
            status: LogUploadStatus201::Idle,
            request_id: None,
        }
    );
}

#[test]
fn malformed_notifications_are_property_violations() {
    for payload in [
        json!({"status":"Uploaded"}),
        json!({"status":"Uploading"}),
        json!({"status":"AcceptedCanceled"}),
        json!({"status":"Uploading","requestId":null}),
        json!({"status":"Uploaded","requestId":2_147_483_648_i64}),
        json!({"status":"Uploaded","requestId":"1"}),
        // `UploadFailed` belongs to the 1.6 DiagnosticsStatusNotification only.
        json!({"status":"UploadFailed","requestId":1}),
        json!({"status":"uploaded","requestId":1}),
        json!({"status":"Uploaded","requestId":1,"fileName":"x"}),
        json!({"status":"Uploaded","requestId":1,"customData":{}}),
        json!({}),
    ] {
        let error = decode_call(&frame(&payload)).unwrap_err();
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
        if fixture["protocol_version"] != "2.0.1"
            || fixture["message_type"] != 2
            || fixture["action"] != "LogStatusNotification"
        {
            continue;
        }
        let bytes = std::fs::read(corpus().join(fixture["wire"].as_str().unwrap())).unwrap();
        let call = decode_call(&bytes).unwrap_or_else(|_| panic!("{}", fixture["id"]));
        assert!(
            matches!(call.observation, ChargerObservation::LogStatus201 { .. }),
            "{}",
            fixture["id"]
        );
        decoded += 1;
    }
    // Eight identified statuses, an identity-free Idle, plus the trigger results.
    assert!(decoded >= 10, "{decoded}");
    let cases: Value = serde_json::from_slice(
        &std::fs::read(corpus().join("wire/2.0.1/log-negative-cases.json")).unwrap(),
    )
    .unwrap();
    let mut refused = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][0] != 2 || case["wire"][2] != "LogStatusNotification" {
            continue;
        }
        let bytes = serde_json::to_vec(&case["wire"]).unwrap();
        assert!(decode_call(&bytes).is_err(), "{}", case["id"]);
        refused += 1;
    }
    assert!(refused > 0);
}
