use jsonschema::Draft;
use serde_json::{Value, json};

fn schema(name: &str) -> jsonschema::Validator {
    let (directory, draft) = if name.starts_with("Signed") {
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
        "../corpus/wire/1.6/firmware-negative-cases.json"
    ))
    .unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(cases.len() >= 14);
    for case in cases {
        let wire = &case["wire"];
        let validator = schema(wire[2].as_str().unwrap());
        assert_eq!(
            validator.is_valid(&wire[3]),
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
    let legacy = schema("FirmwareStatusNotification");
    let signed = schema("SignedFirmwareStatusNotification");
    for status in [
        "Downloaded",
        "DownloadFailed",
        "Downloading",
        "Idle",
        "InstallationFailed",
        "Installing",
        "Installed",
    ] {
        assert!(legacy.is_valid(&json!({"status":status})));
        assert!(signed.is_valid(&json!({"status":status,"requestId":1})));
    }
    for status in [
        "DownloadScheduled",
        "DownloadPaused",
        "InstallRebooting",
        "InstallScheduled",
        "InstallVerificationFailed",
        "InvalidSignature",
        "SignatureVerified",
    ] {
        assert!(!legacy.is_valid(&json!({"status":status})));
        assert!(signed.is_valid(&json!({"status":status,"requestId":1})));
    }
    let reply = schema("SignedUpdateFirmwareResponse");
    for status in [
        "Accepted",
        "Rejected",
        "AcceptedCanceled",
        "InvalidCertificate",
        "RevokedCertificate",
    ] {
        assert!(reply.is_valid(&json!({"status":status})));
    }
    assert!(!reply.is_valid(&json!({"status":"Scheduled"})));
    assert!(!reply.is_valid(&json!({})));
    // The legacy reply carries no status at all; 1.6 defines no rejection.
    let legacy_reply = schema("UpdateFirmwareResponse");
    assert!(legacy_reply.is_valid(&json!({})));
    assert!(!legacy_reply.is_valid(&json!({"status":"Accepted"})));
}
