use serde_json::{Value, json};
use uob_contracts::*;

fn validator(source: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(source).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

fn legacy() -> Value {
    json!({"artifactReference":"station-fw_1.2.bin","retrieveDate":"2099-01-01T00:00:00Z","retries":3,"retryInterval":60})
}

fn signed() -> Value {
    json!({"requestId":i32::MIN,"artifactReference":"signed-fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z","installDateTime":"2099-01-01T01:00:00Z"})
}

#[test]
fn reference_requests_never_carry_station_locations_or_signing_material() {
    let legacy_schema = validator(include_str!(
        "../schemas/v1.0/update-firmware-reference-16.schema.json"
    ));
    let signed_schema = validator(include_str!(
        "../schemas/v1.0/signed-update-firmware-reference-16.schema.json"
    ));
    assert!(legacy_schema.is_valid(&legacy()));
    assert!(signed_schema.is_valid(&signed()));
    let decoded: UpdateFirmwareReference16 = serde_json::from_value(legacy()).unwrap();
    assert_eq!(decoded.retries, Some(3));
    let decoded: SignedUpdateFirmwareReference16 = serde_json::from_value(signed()).unwrap();
    assert_eq!(decoded.request_id, i32::MIN);
    for field in [
        "location",
        "signingCertificate",
        "signature",
        "firmware",
        "extra",
    ] {
        let mut value = legacy();
        value[field] = json!("http://example.invalid/fw.bin");
        assert!(!legacy_schema.is_valid(&value), "{field}");
        assert!(serde_json::from_value::<UpdateFirmwareReference16>(value).is_err());
        let mut value = signed();
        value[field] = json!("-----BEGIN CERTIFICATE-----");
        assert!(!signed_schema.is_valid(&value), "{field}");
        assert!(serde_json::from_value::<SignedUpdateFirmwareReference16>(value).is_err());
    }
    for reference in [".hidden", "", "a/b", "a b", &"x".repeat(129)] {
        let mut value = legacy();
        value["artifactReference"] = json!(reference);
        assert!(!legacy_schema.is_valid(&value), "{reference}");
        assert!(serde_json::from_value::<UpdateFirmwareReference16>(value).is_err());
    }
    for count in [json!(-1), json!(2_147_483_648_u64)] {
        let mut value = signed();
        value["retries"] = count.clone();
        assert!(!signed_schema.is_valid(&value));
        assert!(serde_json::from_value::<SignedUpdateFirmwareReference16>(value).is_err());
        let mut value = legacy();
        value["retryInterval"] = count;
        assert!(serde_json::from_value::<UpdateFirmwareReference16>(value).is_err());
    }
    // A schema cannot compare instants; the contract still refuses an install before retrieval.
    let mut reversed = signed();
    reversed["installDateTime"] = json!("2098-12-31T23:59:59Z");
    assert!(signed_schema.is_valid(&reversed));
    assert!(serde_json::from_value::<SignedUpdateFirmwareReference16>(reversed).is_err());
}

fn job(state: FirmwareJobState16, at: UtcTimestamp) -> FirmwareJob16 {
    FirmwareJob16 {
        revision: 3,
        state,
        deadline: at,
        observed_at: at,
        last_status: Some(FirmwareStatus16::Downloading),
        last_status_at: Some(at),
        notifications: 2,
        rejected_transitions: 1,
    }
}

#[test]
fn every_reply_and_job_state_survives_results_and_nested_exports() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let mut result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    assert!(result.firmware_16.is_none());
    let results = validator(include_str!("../schemas/v1.14/command-result.schema.json"));
    let exports = validator(include_str!("../schemas/v1.15/export-record.schema.json"));
    let artifact = FirmwareArtifact16 {
        artifact_reference: "signed-fw.bin".to_owned(),
        sha256: "0f".repeat(32),
        size_bytes: 1,
        signed: true,
        test_only: true,
    };
    let replies = [
        FirmwareReply16::Acknowledged,
        FirmwareReply16::CallError {
            code: FirmwareCallError16::NotSupported,
        },
    ]
    .into_iter()
    .chain(
        [
            SignedUpdateFirmwareStatus16::Accepted,
            SignedUpdateFirmwareStatus16::Rejected,
            SignedUpdateFirmwareStatus16::AcceptedCanceled,
            SignedUpdateFirmwareStatus16::InvalidCertificate,
            SignedUpdateFirmwareStatus16::RevokedCertificate,
        ]
        .map(|status| FirmwareReply16::Status { status }),
    );
    for reply in replies {
        let evidence = FirmwareResult16::SignedUpdateFirmware {
            request_id: i32::MAX,
            artifact: Some(artifact.clone()),
            reply: Some(reply),
            job: job(FirmwareJobState16::Accepted, result.recorded_at),
        };
        result.schema_version = ContractVersion::V1_FIRMWARE_16;
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        };
        result.firmware_16 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(results.is_valid(&encoded), "{reply:?}");
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .firmware_16,
            Some(evidence)
        );
        let batch: ExportBatch =
            serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(result.clone()),
        );
        assert_eq!(record.metadata().schema_version.revision, 15);
        assert!(exports.is_valid(&serde_json::to_value(record).unwrap()));
    }
    let states: Vec<FirmwareJobState16> = serde_json::from_value(json!([
        "pending",
        "uncertain",
        "accepted",
        "download_scheduled",
        "downloading",
        "download_paused",
        "downloaded",
        "signature_verified",
        "install_scheduled",
        "install_rebooting",
        "installing",
        "timed_out",
        "installed",
        "download_failed",
        "installation_failed",
        "install_verification_failed",
        "invalid_signature",
        "rejected",
        "not_sent",
        "cancelled",
        "superseded",
        "station_idle"
    ]))
    .unwrap();
    let resolved: Vec<_> = states.iter().filter(|state| state.resolved()).collect();
    assert_eq!(resolved.len(), 10);
    assert!(!FirmwareJobState16::TimedOut.resolved());
    for state in states {
        let evidence = FirmwareResult16::UpdateFirmware {
            artifact: None,
            reply: None,
            job: job(state, result.recorded_at),
        };
        result.lifecycle = CommandLifecycle::Dispatched;
        result.firmware_16 = Some(evidence);
        assert!(results.is_valid(&serde_json::to_value(&result).unwrap()));
    }
}

#[test]
fn public_evidence_rejects_private_fields() {
    let value = json!({"action":"UpdateFirmware","reply":{"kind":"acknowledged"},"job":{"revision":1,"state":"accepted","deadline":"2099-01-01T00:00:00Z","observed_at":"2099-01-01T00:00:00Z","notifications":0,"rejected_transitions":0}});
    assert!(serde_json::from_value::<FirmwareResult16>(value.clone()).is_ok());
    for field in ["location", "signingCertificate", "signature", "payload"] {
        let mut private = value.clone();
        private[field] = json!("PRIVATE-MARKER");
        assert!(serde_json::from_value::<FirmwareResult16>(private).is_err());
    }
    let legacy = FirmwareStatus16::Installed;
    assert!(legacy.legacy());
    assert!(!FirmwareStatus16::SignatureVerified.legacy());
    assert_eq!(
        FirmwareCallError16::from_code("Unrecognized"),
        FirmwareCallError16::GenericError
    );
}
