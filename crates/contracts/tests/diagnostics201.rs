use serde_json::{Value, json};
use uob_contracts::*;

fn validator(source: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(source).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

fn log() -> Value {
    json!({"logType":"SecurityLog","requestId":i32::MIN,"oldestTimestamp":"2099-01-01T00:00:00Z","latestTimestamp":"2099-01-01T01:00:00Z","retries":3,"retryInterval":60})
}

#[test]
fn reference_requests_never_carry_upload_locations() {
    let schema = validator(include_str!(
        "../schemas/v1.0/get-log-reference-201.schema.json"
    ));
    assert!(schema.is_valid(&log()));
    assert!(schema.is_valid(&json!({"logType":"DiagnosticsLog","requestId":0})));
    let decoded: GetLogReference201 = serde_json::from_value(log()).unwrap();
    assert_eq!(decoded.request_id, i32::MIN);
    assert_eq!(decoded.log_type, LogType201::SecurityLog);
    assert_eq!(decoded.retries, Some(3));
    for field in ["location", "remoteLocation", "log", "extra", "customData"] {
        let mut value = log();
        value[field] = json!({"remoteLocation": "ftp://user:secret@example.invalid/"});
        assert!(!schema.is_valid(&value), "{field}");
        assert!(serde_json::from_value::<GetLogReference201>(value).is_err());
    }
    for count in [json!(-1), json!(2_147_483_648_u64)] {
        let mut value = log();
        value["retries"] = count.clone();
        assert!(!schema.is_valid(&value));
        assert!(serde_json::from_value::<GetLogReference201>(value).is_err());
        let mut value = log();
        value["retryInterval"] = count;
        assert!(serde_json::from_value::<GetLogReference201>(value).is_err());
    }
    for missing in ["logType", "requestId"] {
        let mut value = log();
        value.as_object_mut().unwrap().remove(missing);
        assert!(!schema.is_valid(&value), "{missing}");
        assert!(serde_json::from_value::<GetLogReference201>(value).is_err());
    }
    for request_id in [json!(2_147_483_648_u64), json!("7"), json!(1.5)] {
        let mut value = log();
        value["requestId"] = request_id;
        assert!(serde_json::from_value::<GetLogReference201>(value).is_err());
    }
    let mut value = log();
    value["logType"] = json!("FirmwareLog");
    assert!(!schema.is_valid(&value));
    assert!(serde_json::from_value::<GetLogReference201>(value).is_err());
    // A schema cannot compare instants; the contract still refuses an inverted window.
    let mut reversed = log();
    reversed["latestTimestamp"] = json!("2098-12-31T23:59:59Z");
    assert!(schema.is_valid(&reversed));
    assert!(serde_json::from_value::<GetLogReference201>(reversed).is_err());
}

fn job(state: DiagnosticsJobState201, at: UtcTimestamp) -> DiagnosticsJob201 {
    DiagnosticsJob201 {
        revision: 3,
        state,
        deadline: at,
        observed_at: at,
        last_status: Some(LogUploadStatus201::Uploading),
        last_status_at: Some(at),
        notifications: 2,
        upload: Some(DiagnosticsUpload201 {
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
    assert!(result.diagnostics_201.is_none());
    let results = validator(include_str!("../schemas/v1.17/command-result.schema.json"));
    let exports = validator(include_str!("../schemas/v1.18/export-record.schema.json"));
    let destination = DiagnosticsDestination201 {
        log_type: LogType201::SecurityLog,
        maximum_bytes: 1024,
        test_only: true,
    };
    let replies = [
        DiagnosticsReply201::CallError {
            code: DiagnosticsCallError201::NotImplemented,
        },
        DiagnosticsReply201::Status {
            status: GetLogStatus201::Rejected,
            file_name: None,
            reason_code: Some("NoLogs".to_owned()),
        },
    ]
    .into_iter()
    .chain(
        [GetLogStatus201::Accepted, GetLogStatus201::AcceptedCanceled].map(|status| {
            DiagnosticsReply201::Status {
                status,
                file_name: Some("security.log".to_owned()),
                reason_code: None,
            }
        }),
    );
    for reply in replies {
        let evidence = DiagnosticsResult201 {
            log_type: LogType201::SecurityLog,
            request_id: i32::MAX,
            destination: Some(destination),
            reply: Some(reply.clone()),
            job: job(DiagnosticsJobState201::Accepted, result.recorded_at),
        };
        result.schema_version = ContractVersion::V1_DIAGNOSTICS_201;
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        };
        result.diagnostics_201 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(results.is_valid(&encoded), "{reply:?}");
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .diagnostics_201,
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
    let states: Vec<DiagnosticsJobState201> = serde_json::from_value(json!([
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
        "rejected",
        "not_sent",
        "cancelled",
        "superseded",
        "station_idle"
    ]))
    .unwrap();
    assert_eq!(states.iter().filter(|state| state.resolved()).count(), 11);
    assert!(!DiagnosticsJobState201::TimedOut.resolved());
    for state in states {
        result.lifecycle = CommandLifecycle::Dispatched;
        result.diagnostics_201 = Some(DiagnosticsResult201 {
            log_type: LogType201::DiagnosticsLog,
            request_id: 1,
            destination: None,
            reply: None,
            job: job(state, result.recorded_at),
        });
        assert!(results.is_valid(&serde_json::to_value(&result).unwrap()));
    }
}

#[test]
fn public_evidence_rejects_private_fields_and_classifies_native_values() {
    let value = json!({"log_type":"DiagnosticsLog","request_id":4,"reply":{"kind":"status","status":"Accepted","file_name":"d.log"},"job":{"revision":1,"state":"accepted","deadline":"2099-01-01T00:00:00Z","observed_at":"2099-01-01T00:00:00Z","notifications":0}});
    let decoded: DiagnosticsResult201 = serde_json::from_value(value.clone()).unwrap();
    assert!(decoded.accepted());
    for field in ["location", "remoteLocation", "upload_id", "payload"] {
        let mut private = value.clone();
        private[field] = json!("PRIVATE-MARKER");
        assert!(serde_json::from_value::<DiagnosticsResult201>(private).is_err());
    }
    let mut private = value;
    private["job"]["upload"] = json!({"sha256":"0f".repeat(32),"size_bytes":1,"location":"x"});
    assert!(serde_json::from_value::<DiagnosticsResult201>(private).is_err());
    assert!(
        !DiagnosticsReply201::Status {
            status: GetLogStatus201::Rejected,
            file_name: None,
            reason_code: None
        }
        .accepted()
    );
    let canceled = DiagnosticsReply201::Status {
        status: GetLogStatus201::AcceptedCanceled,
        file_name: None,
        reason_code: None,
    };
    assert!(canceled.accepted() && canceled.cancelled_previous());
    assert!(valid_log_reason_code_201("NoLogs"));
    assert!(!valid_log_reason_code_201(&"a".repeat(21)));
    assert!(!valid_log_reason_code_201("with space"));
    assert!(!valid_log_reason_code_201(""));
    assert_eq!(LogUploadStatus201::UploadFailure.as_str(), "UploadFailure");
    // The 1.6-only `UploadFailed` is not a 2.0.1 value.
    assert!(serde_json::from_value::<LogUploadStatus201>(json!("UploadFailed")).is_err());
    assert_eq!(
        DiagnosticsCallError201::from_code("Unrecognized"),
        DiagnosticsCallError201::GenericError
    );
}
