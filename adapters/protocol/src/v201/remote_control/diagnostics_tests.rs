use super::native_reply;
use serde_json::json;
use uob_contracts::{DiagnosticsReply201, GetLogStatus201};

fn status(
    status: GetLogStatus201,
    file: Option<&str>,
    reason: Option<&str>,
) -> DiagnosticsReply201 {
    DiagnosticsReply201::Status {
        status,
        file_name: file.map(str::to_owned),
        reason_code: reason.map(str::to_owned),
    }
}

#[test]
fn every_native_status_file_name_and_reason_code_is_kept_exactly() {
    for (name, native) in [
        ("Accepted", GetLogStatus201::Accepted),
        ("Rejected", GetLogStatus201::Rejected),
        ("AcceptedCanceled", GetLogStatus201::AcceptedCanceled),
    ] {
        assert_eq!(
            native_reply(&json!({"status": name})),
            Some(status(native, None, None))
        );
        assert_eq!(
            native_reply(&json!({"status": name, "filename": "diag.zip"})),
            Some(status(native, Some("diag.zip"), None))
        );
    }
    // additionalInfo is free station text: validated, but never retained.
    assert_eq!(
        native_reply(&json!({
            "status": "Rejected",
            "filename": "a.log",
            "statusInfo": {"reasonCode": "NoLogs", "additionalInfo": "busy", "customData": {"vendorId": "v"}},
            "customData": {"vendorId": "v"}
        })),
        Some(status(
            GetLogStatus201::Rejected,
            Some("a.log"),
            Some("NoLogs")
        ))
    );
}

#[test]
fn malformed_replies_leave_delivery_uncertain() {
    for payload in [
        json!({}),
        json!([]),
        json!({"status": "accepted"}),
        json!({"status": "Accepted", "extra": 1}),
        json!({"status": "Accepted", "fileName": "wrong-case"}),
        json!({"status": "Accepted", "filename": 7}),
        json!({"status": "Accepted", "filename": null}),
        json!({"status": "Accepted", "filename": "x".repeat(256)}),
        json!({"status": "Accepted", "filename": "tab\tname"}),
        json!({"status": "Accepted", "statusInfo": {}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "x".repeat(21)}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "has space"}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "Ok", "additionalInfo": "x".repeat(513)}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "Ok", "other": 1}}),
        json!({"status": "Accepted", "customData": {}}),
    ] {
        assert_eq!(native_reply(&payload), None, "{payload}");
    }
}

#[test]
fn corpus_replies_map_and_negative_replies_are_never_answers() {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1");
    let read = |name: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(corpus.join(name)).unwrap()).unwrap()
    };
    for name in [
        "log-get-reply-accepted.json",
        "log-get-reply-accepted-no-filename.json",
        "log-get-reply-accepted-canceled.json",
        "log-get-reply-rejected.json",
        "log-get-reply-rejected-status-info.json",
    ] {
        assert!(native_reply(&read(name)[2]).is_some(), "{name}");
    }
    let cases = read("log-negative-cases.json");
    let mut refused = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][0] != 3 || !case["schema"].as_str().unwrap().contains("GetLogResponse") {
            continue;
        }
        assert_eq!(native_reply(&case["wire"][2]), None, "{}", case["id"]);
        refused += 1;
    }
    assert!(refused >= 5, "{refused}");
}
