use serde_json::{Value, json};
use uob_contracts::*;

fn validator(source: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(source).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

fn diagnostics() -> Value {
    json!({"startTime":"2099-01-01T00:00:00Z","stopTime":"2099-01-02T00:00:00Z","retries":3,"retryInterval":60})
}

fn log() -> Value {
    json!({"logType":"SecurityLog","requestId":i32::MIN,"oldestTimestamp":"2099-01-01T00:00:00Z","latestTimestamp":"2099-01-01T01:00:00Z"})
}

#[test]
fn reference_requests_never_carry_upload_locations() {
    let diagnostics_schema = validator(include_str!(
        "../schemas/v1.0/get-diagnostics-reference-16.schema.json"
    ));
    let log_schema = validator(include_str!(
        "../schemas/v1.0/get-log-reference-16.schema.json"
    ));
    assert!(diagnostics_schema.is_valid(&diagnostics()));
    assert!(diagnostics_schema.is_valid(&json!({})));
    assert!(log_schema.is_valid(&log()));
    let decoded: GetDiagnosticsReference16 = serde_json::from_value(diagnostics()).unwrap();
    assert_eq!(decoded.retries, Some(3));
    assert_eq!(
        serde_json::from_value::<GetDiagnosticsReference16>(json!({})).unwrap(),
        GetDiagnosticsReference16::default()
    );
    let decoded: GetLogReference16 = serde_json::from_value(log()).unwrap();
    assert_eq!(decoded.request_id, i32::MIN);
    assert_eq!(decoded.log_type, LogType16::SecurityLog);
    for field in ["location", "remoteLocation", "log", "extra"] {
        let mut value = diagnostics();
        value[field] = json!("ftp://user:secret@example.invalid/");
        assert!(!diagnostics_schema.is_valid(&value), "{field}");
        assert!(serde_json::from_value::<GetDiagnosticsReference16>(value).is_err());
        let mut value = log();
        value[field] = json!({"remoteLocation":"ftp://example.invalid/"});
        assert!(!log_schema.is_valid(&value), "{field}");
        assert!(serde_json::from_value::<GetLogReference16>(value).is_err());
    }
    for count in [json!(-1), json!(2_147_483_648_u64)] {
        let mut value = log();
        value["retries"] = count.clone();
        assert!(!log_schema.is_valid(&value));
        assert!(serde_json::from_value::<GetLogReference16>(value).is_err());
        let mut value = diagnostics();
        value["retryInterval"] = count;
        assert!(serde_json::from_value::<GetDiagnosticsReference16>(value).is_err());
    }
    let mut value = log();
    value["logType"] = json!("FirmwareLog");
    assert!(!log_schema.is_valid(&value));
    assert!(serde_json::from_value::<GetLogReference16>(value).is_err());
    // A schema cannot compare instants; the contract still refuses an inverted window.
    let mut reversed = diagnostics();
    reversed["stopTime"] = json!("2098-12-31T23:59:59Z");
    assert!(diagnostics_schema.is_valid(&reversed));
    assert!(serde_json::from_value::<GetDiagnosticsReference16>(reversed).is_err());
    let mut reversed = log();
    reversed["latestTimestamp"] = json!("2098-12-31T23:59:59Z");
    assert!(serde_json::from_value::<GetLogReference16>(reversed).is_err());
}

fn job(state: DiagnosticsJobState16, at: UtcTimestamp) -> DiagnosticsJob16 {
    DiagnosticsJob16 {
        revision: 3,
        state,
        deadline: at,
        observed_at: at,
        last_status: Some(LogUploadStatus16::Uploading),
        last_status_at: Some(at),
        notifications: 2,
        rejected_transitions: 1,
        upload: Some(DiagnosticsUpload16 {
            sha256: "0f".repeat(32),
            size_bytes: 42,
        }),
    }
}

#[test]
fn every_reply_and_job_state_survives_results_and_nested_exports() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let mut result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    assert!(result.diagnostics_16.is_none());
    let results = validator(include_str!("../schemas/v1.16/command-result.schema.json"));
    let exports = validator(include_str!("../schemas/v1.17/export-record.schema.json"));
    let destination = DiagnosticsDestination16 {
        log_type: LogType16::DiagnosticsLog,
        maximum_bytes: 1024,
        test_only: true,
    };
    let replies = [
        DiagnosticsReply16::Diagnostics { file_name: None },
        DiagnosticsReply16::Diagnostics {
            file_name: Some("diagnostics-1.zip".to_owned()),
        },
        DiagnosticsReply16::CallError {
            code: DiagnosticsCallError16::NotImplemented,
        },
    ]
    .into_iter()
    .chain(
        [
            GetLogStatus16::Accepted,
            GetLogStatus16::Rejected,
            GetLogStatus16::AcceptedCanceled,
        ]
        .map(|status| DiagnosticsReply16::Log {
            status,
            file_name: Some("security.log".to_owned()),
        }),
    );
    for reply in replies {
        let evidence = DiagnosticsResult16::GetLog {
            log_type: LogType16::SecurityLog,
            request_id: i32::MAX,
            destination: Some(destination),
            reply: Some(reply.clone()),
            job: job(DiagnosticsJobState16::Accepted, result.recorded_at),
        };
        result.schema_version = ContractVersion::V1_DIAGNOSTICS_16;
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        };
        result.diagnostics_16 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(results.is_valid(&encoded), "{reply:?}");
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .diagnostics_16,
            Some(evidence)
        );
        let batch: ExportBatch =
            serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(result.clone()),
        );
        assert_eq!(record.metadata().schema_version.revision, 16);
        assert!(exports.is_valid(&serde_json::to_value(record).unwrap()));
    }
    let states: Vec<DiagnosticsJobState16> = serde_json::from_value(json!([
        "pending",
        "uncertain",
        "accepted",
        "uploading",
        "timed_out",
        "uploaded",
        "upload_unconfirmed",
        "upload_failed",
        "bad_message",
        "not_supported_operation",
        "permission_denied",
        "no_log_available",
        "rejected",
        "not_sent",
        "cancelled",
        "superseded",
        "station_idle"
    ]))
    .unwrap();
    let resolved: Vec<_> = states.iter().filter(|state| state.resolved()).collect();
    assert_eq!(resolved.len(), 12);
    assert!(!DiagnosticsJobState16::TimedOut.resolved());
    for state in states {
        let evidence = DiagnosticsResult16::GetDiagnostics {
            destination: None,
            reply: None,
            job: job(state, result.recorded_at),
        };
        result.lifecycle = CommandLifecycle::Dispatched;
        result.diagnostics_16 = Some(evidence);
        assert!(results.is_valid(&serde_json::to_value(&result).unwrap()));
    }
}

#[test]
fn public_evidence_rejects_private_fields_and_classifies_native_values() {
    let value = json!({"action":"GetDiagnostics","reply":{"kind":"diagnostics","file_name":"d.log"},"job":{"revision":1,"state":"accepted","deadline":"2099-01-01T00:00:00Z","observed_at":"2099-01-01T00:00:00Z","notifications":0,"rejected_transitions":0}});
    let decoded: DiagnosticsResult16 = serde_json::from_value(value.clone()).unwrap();
    assert!(decoded.accepted());
    for field in ["location", "remoteLocation", "upload_id", "payload"] {
        let mut private = value.clone();
        private[field] = json!("PRIVATE-MARKER");
        assert!(serde_json::from_value::<DiagnosticsResult16>(private).is_err());
    }
    let mut private = value;
    private["job"]["upload"] = json!({"sha256":"0f".repeat(32),"size_bytes":1,"location":"x"});
    assert!(serde_json::from_value::<DiagnosticsResult16>(private).is_err());
    assert!(!DiagnosticsReply16::Diagnostics { file_name: None }.accepted());
    assert!(
        DiagnosticsReply16::Log {
            status: GetLogStatus16::AcceptedCanceled,
            file_name: None
        }
        .accepted()
    );
    let diagnostics: Vec<_> = [
        LogUploadStatus16::BadMessage,
        LogUploadStatus16::Idle,
        LogUploadStatus16::NotSupportedOperation,
        LogUploadStatus16::PermissionDenied,
        LogUploadStatus16::Uploaded,
        LogUploadStatus16::UploadFailed,
        LogUploadStatus16::UploadFailure,
        LogUploadStatus16::Uploading,
    ]
    .into_iter()
    .filter(|status| status.diagnostics())
    .collect();
    assert_eq!(diagnostics.len(), 4);
    assert!(!LogUploadStatus16::UploadFailed.log());
    assert!(!LogUploadStatus16::UploadFailure.diagnostics());
    assert!(valid_log_file_name(&"a".repeat(255)));
    assert!(!valid_log_file_name(&"a".repeat(256)));
    assert!(!valid_log_file_name("line\nbreak"));
    assert_eq!(
        DiagnosticsCallError16::from_code("Unrecognized"),
        DiagnosticsCallError16::GenericError
    );
}
