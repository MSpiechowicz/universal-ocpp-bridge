use serde_json::{Value, json};
use uob_application::ChargerObservation;
use uob_contracts::FirmwareStatus16;
use uob_protocol_adapter::{DecodeErrorKind, v16::decode_call};

fn frame(payload: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!([
        2,
        "signed-1",
        "SignedFirmwareStatusNotification",
        payload
    ]))
    .unwrap()
}

const STATUSES: [&str; 14] = [
    "Downloaded",
    "DownloadFailed",
    "Downloading",
    "DownloadScheduled",
    "DownloadPaused",
    "Idle",
    "InstallationFailed",
    "Installing",
    "Installed",
    "InstallRebooting",
    "InstallScheduled",
    "InstallVerificationFailed",
    "InvalidSignature",
    "SignatureVerified",
];

#[test]
fn every_whitepaper_status_decodes_with_its_signed_request_identity() {
    for status in STATUSES {
        for request_id in [i32::MIN, 0, i32::MAX] {
            let decoded =
                decode_call(&frame(&json!({"status":status,"requestId":request_id}))).unwrap();
            assert_eq!(decoded.action.as_str(), "SignedFirmwareStatusNotification");
            assert_eq!(
                decoded.observation,
                ChargerObservation::SignedFirmwareStatus16 {
                    status: serde_json::from_value::<FirmwareStatus16>(json!(status)).unwrap(),
                    request_id: Some(request_id),
                }
            );
        }
    }
    // L01.FR.21: only Idle may omit the update identity.
    let idle = decode_call(&frame(&json!({"status":"Idle"}))).unwrap();
    assert_eq!(
        idle.observation,
        ChargerObservation::SignedFirmwareStatus16 {
            status: FirmwareStatus16::Idle,
            request_id: None,
        }
    );
}

#[test]
fn malformed_signed_notifications_are_property_violations() {
    for payload in [
        json!({"status":"Installed"}),
        json!({"status":"Installing","requestId":null}),
        json!({"status":"Installed","requestId":2_147_483_648_i64}),
        json!({"status":"Installed","requestId":"1"}),
        json!({"status":"InvalidCertificate","requestId":1}),
        json!({"status":"installed","requestId":1}),
        json!({"status":"Installed","requestId":1,"firmwareVersion":"x"}),
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
fn every_independent_inbound_firmware_fixture_decodes_and_negative_cases_do_not() {
    let registry: Value =
        serde_json::from_slice(&std::fs::read(corpus().join("fixtures.json")).unwrap()).unwrap();
    let mut decoded = 0;
    for fixture in registry["fixtures"].as_array().unwrap() {
        let action = fixture["action"].as_str().unwrap();
        if fixture["message_type"] != 2
            || !matches!(
                action,
                "FirmwareStatusNotification" | "SignedFirmwareStatusNotification"
            )
        {
            continue;
        }
        let bytes = std::fs::read(corpus().join(fixture["wire"].as_str().unwrap())).unwrap();
        let call = decode_call(&bytes).unwrap_or_else(|_| panic!("{}", fixture["id"]));
        assert_eq!(call.action.as_str(), action);
        decoded += 1;
    }
    // Seven legacy and fifteen signed notifications, plus earlier trigger-result fixtures.
    assert!(decoded >= 22);
    let cases: Value = serde_json::from_slice(
        &std::fs::read(corpus().join("wire/1.6/firmware-negative-cases.json")).unwrap(),
    )
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        if !case["wire"][2]
            .as_str()
            .unwrap()
            .ends_with("StatusNotification")
        {
            continue;
        }
        let bytes = serde_json::to_vec(&case["wire"]).unwrap();
        assert!(decode_call(&bytes).is_err(), "{}", case["id"]);
    }
}
