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

fn request_schema(action: &str) -> &'static str {
    match action {
        "UpdateFirmware" => "UpdateFirmwareRequest",
        "FirmwareStatusNotification" => "FirmwareStatusNotificationRequest",
        other => panic!("unexpected negative-case action {other}"),
    }
}

#[test]
fn negative_cases_separate_schema_floor_from_native_semantics() {
    let cases: Value = serde_json::from_str(include_str!(
        "../corpus/wire/2.0.1/firmware-negative-cases.json"
    ))
    .unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(cases.len() >= 16);
    for case in cases {
        let wire = &case["wire"];
        let validator = schema(request_schema(wire[2].as_str().unwrap()));
        assert_eq!(
            validator.is_valid(&wire[3]),
            case["schema_valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
        // No case is accepted natively; schema-valid ones prove semantics above the schema.
        assert!(!case["native_valid"].as_bool().unwrap(), "{}", case["id"]);
    }
}

#[test]
fn one_message_carries_secure_and_non_secure_updates_with_exact_status_sets() {
    let request = schema("UpdateFirmwareRequest");
    let firmware =
        json!({"location":"http://127.0.0.1:9/fw.bin","retrieveDateTime":"2026-10-07T12:00:00Z"});
    // L02 omits the signing material; L01 carries both values in the same message.
    assert!(request.is_valid(&json!({"requestId":1,"firmware":firmware})));
    let mut secure = firmware.clone();
    secure["signingCertificate"] = "-----BEGIN CERTIFICATE-----".into();
    secure["signature"] = "c2ln".into();
    assert!(request.is_valid(&json!({"requestId":1,"firmware":secure})));
    assert!(!request.is_valid(&json!({"firmware":firmware})));

    let notification = schema("FirmwareStatusNotificationRequest");
    for status in [
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
    ] {
        assert!(notification.is_valid(&json!({"status":status,"requestId":7})));
        // The schema leaves requestId optional; L01.FR.20 narrows it natively.
        assert!(notification.is_valid(&json!({ "status": status })));
    }
    assert!(!notification.is_valid(&json!({"status":"InvalidCertificate","requestId":7})));

    let reply = schema("UpdateFirmwareResponse");
    for status in [
        "Accepted",
        "Rejected",
        "AcceptedCanceled",
        "InvalidCertificate",
        "RevokedCertificate",
    ] {
        assert!(reply.is_valid(&json!({ "status": status })));
    }
    assert!(reply.is_valid(&json!({"status":"Rejected","statusInfo":{"reasonCode":"Busy"}})));
    assert!(!reply.is_valid(&json!({"status":"Rejected","statusInfo":{}})));
    assert!(!reply.is_valid(&json!({})));
    let empty = schema("FirmwareStatusNotificationResponse");
    assert!(empty.is_valid(&json!({})));
    assert!(!empty.is_valid(&json!({"status":"Accepted"})));
}

#[test]
fn every_registered_firmware_fixture_has_the_expected_family() {
    let registry: Value = serde_json::from_str(include_str!("../corpus/fixtures.json")).unwrap();
    let ids = registry["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|fixture| fixture["id"].as_str())
        .filter(|id| {
            id.starts_with("wire.ocpp201.firmware-update")
                || id.starts_with("wire.ocpp201.firmware-status")
        })
        .count();
    // Four requests, six replies, fifteen notifications and the empty acknowledgement.
    assert_eq!(ids, 26);
}
