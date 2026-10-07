use serde_json::{Value, json};
use uob_contracts::*;

fn validator(source: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(source).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

fn reference() -> Value {
    json!({"requestId":i32::MIN,"artifactReference":"station-fw_2.0.bin","retrieveDateTime":"2099-01-01T00:00:00Z","installDateTime":"2099-01-01T01:00:00Z","retries":3,"retryInterval":60})
}

#[test]
fn reference_requests_never_carry_station_locations_or_signing_material() {
    let schema = validator(include_str!(
        "../schemas/v1.0/update-firmware-reference-201.schema.json"
    ));
    assert!(schema.is_valid(&reference()));
    let decoded: UpdateFirmwareReference201 = serde_json::from_value(reference()).unwrap();
    assert_eq!(decoded.request_id, i32::MIN);
    assert_eq!(decoded.retries, Some(3));
    let minimal =
        json!({"requestId":1,"artifactReference":"a","retrieveDateTime":"2099-01-01T00:00:00Z"});
    assert!(schema.is_valid(&minimal));
    assert!(serde_json::from_value::<UpdateFirmwareReference201>(minimal).is_ok());
    for field in [
        "location",
        "signingCertificate",
        "signature",
        "firmware",
        "extra",
    ] {
        let mut value = reference();
        value[field] = json!("-----BEGIN CERTIFICATE-----");
        assert!(!schema.is_valid(&value), "{field}");
        assert!(serde_json::from_value::<UpdateFirmwareReference201>(value).is_err());
    }
    for missing in ["requestId", "artifactReference", "retrieveDateTime"] {
        let mut value = reference();
        value.as_object_mut().unwrap().remove(missing);
        assert!(!schema.is_valid(&value), "{missing}");
        assert!(serde_json::from_value::<UpdateFirmwareReference201>(value).is_err());
    }
    for artifact in [".hidden", "", "a/b", "a b", &"x".repeat(129)] {
        let mut value = reference();
        value["artifactReference"] = json!(artifact);
        assert!(!schema.is_valid(&value), "{artifact}");
        assert!(serde_json::from_value::<UpdateFirmwareReference201>(value).is_err());
    }
    for count in [json!(-1), json!(2_147_483_648_u64)] {
        for field in ["retries", "retryInterval"] {
            let mut value = reference();
            value[field] = count.clone();
            assert!(!schema.is_valid(&value));
            assert!(serde_json::from_value::<UpdateFirmwareReference201>(value).is_err());
        }
    }
    // A schema cannot compare instants; the contract still refuses an install before retrieval.
    let mut reversed = reference();
    reversed["installDateTime"] = json!("2098-12-31T23:59:59Z");
    assert!(schema.is_valid(&reversed));
    assert!(serde_json::from_value::<UpdateFirmwareReference201>(reversed).is_err());
}

fn job(state: FirmwareJobState201, at: UtcTimestamp) -> FirmwareJob201 {
    FirmwareJob201 {
        revision: 3,
        state,
        deadline: at,
        observed_at: at,
        last_status: Some(FirmwareStatus201::SignatureVerified),
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
    assert!(result.firmware_201.is_none());
    let results = validator(include_str!("../schemas/v1.15/command-result.schema.json"));
    let exports = validator(include_str!("../schemas/v1.16/export-record.schema.json"));
    let artifact = FirmwareArtifact201 {
        artifact_reference: "station-fw_2.0.bin".to_owned(),
        sha256: "0f".repeat(32),
        size_bytes: 1,
        signed: true,
        test_only: true,
    };
    let replies = [
        FirmwareReply201::CallError {
            code: FirmwareCallError201::RpcFrameworkError,
        },
        FirmwareReply201::Status {
            status: UpdateFirmwareStatus201::RevokedCertificate,
            reason_code: Some("CertRevoked".to_owned()),
        },
    ]
    .into_iter()
    .chain(
        [
            UpdateFirmwareStatus201::Accepted,
            UpdateFirmwareStatus201::Rejected,
            UpdateFirmwareStatus201::AcceptedCanceled,
            UpdateFirmwareStatus201::InvalidCertificate,
            UpdateFirmwareStatus201::RevokedCertificate,
        ]
        .map(|status| FirmwareReply201::Status {
            status,
            reason_code: None,
        }),
    );
    for reply in replies {
        let evidence = FirmwareResult201 {
            request_id: i32::MAX,
            secure: true,
            artifact: Some(artifact.clone()),
            reply: Some(reply.clone()),
            job: job(FirmwareJobState201::Accepted, result.recorded_at),
        };
        result.schema_version = ContractVersion::V1_FIRMWARE_201;
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        };
        result.firmware_201 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(results.is_valid(&encoded), "{reply:?}");
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .firmware_201,
            Some(evidence)
        );
        let batch: ExportBatch =
            serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(result.clone()),
        );
        assert_eq!(record.metadata().schema_version.revision, 17);
        assert!(exports.is_valid(&serde_json::to_value(record).unwrap()));
    }
}

#[test]
fn every_job_state_is_public_and_only_end_states_are_resolved() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let mut result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    result.schema_version = ContractVersion::V1_FIRMWARE_201;
    let results = validator(include_str!("../schemas/v1.15/command-result.schema.json"));
    let states: Vec<FirmwareJobState201> = serde_json::from_value(json!([
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
    assert_eq!(states.iter().filter(|state| state.resolved()).count(), 10);
    assert!(!FirmwareJobState201::TimedOut.resolved());
    for state in states {
        result.lifecycle = CommandLifecycle::Dispatched;
        result.firmware_201 = Some(FirmwareResult201 {
            request_id: 0,
            secure: false,
            artifact: None,
            reply: None,
            job: job(state, result.recorded_at),
        });
        assert!(results.is_valid(&serde_json::to_value(&result).unwrap()));
    }
}

#[test]
fn public_evidence_rejects_private_fields_and_free_text() {
    let value = json!({"request_id":5,"secure":true,"reply":{"kind":"status","status":"Accepted"},"job":{"revision":1,"state":"accepted","deadline":"2099-01-01T00:00:00Z","observed_at":"2099-01-01T00:00:00Z","notifications":0,"rejected_transitions":0}});
    assert!(serde_json::from_value::<FirmwareResult201>(value.clone()).is_ok());
    for field in ["location", "signingCertificate", "signature", "payload"] {
        let mut private = value.clone();
        private[field] = json!("PRIVATE-MARKER");
        assert!(serde_json::from_value::<FirmwareResult201>(private).is_err());
    }
    let mut additional = value;
    additional["reply"]["additional_info"] = json!("station free text");
    assert!(serde_json::from_value::<FirmwareResult201>(additional).is_err());
    assert!(valid_firmware_reason_code_201("InvalidURL"));
    for code in ["", "has space", "\u{e9}", &"x".repeat(21)] {
        assert!(!valid_firmware_reason_code_201(code), "{code}");
    }
    assert_eq!(
        FirmwareCallError201::from_code("FormationViolation"),
        FirmwareCallError201::GenericError
    );
    assert_eq!(
        FirmwareCallError201::from_code("FormatViolation"),
        FirmwareCallError201::FormatViolation
    );
    for status in [
        FirmwareStatus201::Idle,
        FirmwareStatus201::InstallVerificationFailed,
        FirmwareStatus201::SignatureVerified,
    ] {
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!(status.as_str())
        );
    }
}
