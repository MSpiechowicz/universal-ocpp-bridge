//! Native log-upload semantics of the in-memory station model (1.6 §4.4/§5.9, Whitepaper N01).
mod diagnostics16_support;
use diagnostics16_support::{
    attempt, config, drain, get_diagnostics, get_log, names, station, upload,
};
use serde_json::json;
use time::OffsetDateTime;
use uob_sim::diagnostics16::DiagnosticsHandle;

/// Sends a request that must start an upload job.
fn started(station: &DiagnosticsHandle, action: &str, payload: &serde_json::Value) {
    let reply = station.handle_call(action, payload);
    assert!(
        reply.get("fileName").is_some() || reply.get("filename").is_some(),
        "{reply}"
    );
}

#[test]
fn legacy_request_names_a_file_uploads_once_and_reports_uploaded() {
    let station = station(json!({}));
    assert_eq!(station.trigger_status(), "Idle");
    let reply = station.handle_call("GetDiagnostics", &get_diagnostics(json!({})));
    assert_eq!(reply, json!({"fileName": "diagnostics-alpha-1.log"}));
    let ticket = attempt(&station);
    assert_eq!(ticket.file_name, "diagnostics-alpha-1.log");
    assert!(
        ticket
            .content()
            .starts_with(b"UOB-SIM DIAGNOSTICS LOG\nstation=alpha\n")
    );
    assert_eq!(ticket.content().len(), 4096);
    // §4.4: a triggered status while busy is not Idle.
    assert_eq!(station.trigger_status(), "Uploading");
    station
        .upload_finished(&ticket, true, OffsetDateTime::now_utc())
        .unwrap();
    assert_eq!(names(&drain(&station)), ["Uploading", "Uploaded"]);
    assert_eq!(station.trigger_status(), "Idle");
    let snapshot = station.snapshot();
    assert_eq!(snapshot["active"], false);
    assert_eq!(snapshot["uploads"], 1);
    assert_eq!(snapshot["statuses"], json!(["Uploading", "Uploaded"]));
    assert!(snapshot.get("requestId").is_none());
    assert!(!snapshot.to_string().contains("127.0.0.1"));
    assert!(!format!("{ticket:?}").contains("127.0.0.1"));
}

#[test]
fn legacy_station_without_diagnostics_or_busy_answers_without_a_file_name() {
    let empty = station(json!({"no_diagnostics": true}));
    assert_eq!(
        empty.handle_call("GetDiagnostics", &get_diagnostics(json!({}))),
        json!({})
    );
    assert_eq!(empty.snapshot()["active"], false);
    assert!(empty.advance(OffsetDateTime::now_utc()).unwrap().is_none());

    let busy = station(json!({}));
    started(&busy, "GetDiagnostics", &get_diagnostics(json!({})));
    assert_eq!(
        busy.handle_call("GetDiagnostics", &get_diagnostics(json!({}))),
        json!({})
    );
    assert_eq!(busy.snapshot()["fileName"], "diagnostics-alpha-1.log");
}

#[test]
fn bounded_retries_report_uploading_per_attempt_then_a_single_end_state() {
    let recovered = station(json!({"upload_failures": 2}));
    started(
        &recovered,
        "GetDiagnostics",
        &get_diagnostics(json!({"retries": 2, "retryInterval": 0})),
    );
    let first = upload(&recovered, true);
    assert!(first.simulated_failure);
    assert!(upload(&recovered, true).simulated_failure);
    let third = attempt(&recovered);
    assert!(!third.simulated_failure);
    recovered
        .upload_finished(&third, true, OffsetDateTime::now_utc())
        .unwrap();
    assert_eq!(
        names(&drain(&recovered)),
        ["Uploading", "Uploading", "Uploading", "Uploaded"]
    );

    let failed = station(json!({"upload_failures": 2}));
    started(
        &failed,
        "GetDiagnostics",
        &get_diagnostics(json!({"retries": 1, "retryInterval": 0})),
    );
    upload(&failed, false);
    upload(&failed, false);
    assert_eq!(
        names(&drain(&failed)),
        ["Uploading", "Uploading", "UploadFailed"]
    );
    assert_eq!(failed.snapshot()["uploads"], 0);
}

#[test]
fn retry_interval_delays_the_next_attempt() {
    let station = station(json!({}));
    started(
        &station,
        "GetDiagnostics",
        &get_diagnostics(json!({"retries": 1, "retryInterval": 30})),
    );
    upload(&station, false);
    let now = OffsetDateTime::now_utc();
    assert!(station.advance(now).unwrap().is_none());
    assert!(
        station
            .advance(now + time::Duration::seconds(31))
            .unwrap()
            .is_some()
    );
}

#[test]
fn get_log_reports_each_status_with_its_request_id_and_log_type_content() {
    let station = station(json!({"security_log": true}));
    let reply = station.handle_call("GetLog", &get_log("SecurityLog", 7, json!({})));
    assert_eq!(
        reply,
        json!({"status": "Accepted", "filename": "securitylog-alpha-1.log"})
    );
    let ticket = upload(&station, true);
    assert!(ticket.content().starts_with(b"UOB-SIM SECURITY LOG\n"));
    assert_eq!(ticket.content().len(), 2048);
    // N01.FR.07: every status for this upload carries the same requestId.
    assert_eq!(
        drain(&station),
        [
            ("Uploading".to_owned(), Some(7)),
            ("Uploaded".to_owned(), Some(7))
        ]
    );
    // A whitepaper upload never changes the legacy trigger status.
    started(&station, "GetLog", &get_log("DiagnosticsLog", 8, json!({})));
    assert_eq!(station.trigger_status(), "Idle");
    let ticket = attempt(&station);
    assert_eq!(ticket.file_name, "diagnosticslog-alpha-2.log");
    assert!(ticket.content().starts_with(b"UOB-SIM DIAGNOSTICS LOG\n"));
    assert_eq!(station.snapshot()["kind"], "DiagnosticsLog");
}

#[test]
fn get_log_failure_uses_the_configured_n01_fr_10_status_after_every_attempt() {
    for status in [
        "UploadFailure",
        "BadMessage",
        "PermissionDenied",
        "NotSupportedOperation",
    ] {
        let station = station(json!({
            "security_log": true, "legacy": false,
            "upload_failures": 2, "get_log_failure_status": status,
        }));
        started(
            &station,
            "GetLog",
            &get_log(
                "DiagnosticsLog",
                3,
                json!({"retries": 1, "retryInterval": 0}),
            ),
        );
        upload(&station, true);
        upload(&station, true);
        assert_eq!(
            drain(&station),
            [
                ("Uploading".to_owned(), Some(3)),
                ("Uploading".to_owned(), Some(3)),
                (status.to_owned(), Some(3))
            ],
            "{status}"
        );
    }
}

#[test]
fn new_get_log_cancels_or_is_rejected_during_an_ongoing_upload() {
    let cancelling = station(json!({"security_log": true}));
    started(&cancelling, "GetLog", &get_log("SecurityLog", 1, json!({})));
    let stale = attempt(&cancelling);
    // N01.FR.11: cancel the ongoing upload and answer AcceptedCanceled.
    assert_eq!(
        cancelling.handle_call("GetLog", &get_log("DiagnosticsLog", 2, json!({}))),
        json!({"status": "AcceptedCanceled", "filename": "diagnosticslog-alpha-2.log"})
    );
    // The cancelled upload sends no further status, even when its transfer later completes.
    cancelling
        .upload_finished(&stale, true, OffsetDateTime::now_utc())
        .unwrap();
    upload(&cancelling, true);
    assert_eq!(
        drain(&cancelling),
        [
            ("Uploading".to_owned(), Some(2)),
            ("Uploaded".to_owned(), Some(2))
        ]
    );
    let snapshot = cancelling.snapshot();
    assert_eq!(snapshot["cancelled"], json!([1]));
    assert_eq!(snapshot["uploads"], 1);

    let rejecting = station(json!({"security_log": true, "cancel_policy": "reject"}));
    started(&rejecting, "GetLog", &get_log("SecurityLog", 1, json!({})));
    assert_eq!(
        rejecting.handle_call("GetLog", &get_log("SecurityLog", 2, json!({}))),
        json!({"status": "Rejected"})
    );
    assert_eq!(rejecting.snapshot()["requestId"], 1);

    // A GetLog also cancels an ongoing legacy diagnostics upload.
    let both = station(json!({"security_log": true}));
    started(&both, "GetDiagnostics", &get_diagnostics(json!({})));
    assert_eq!(
        both.handle_call("GetLog", &get_log("SecurityLog", 9, json!({})))["status"],
        "AcceptedCanceled"
    );
    assert_eq!(both.snapshot()["cancelled"], json!([]));
}

#[test]
fn rejecting_station_and_unsupported_families_start_nothing() {
    let rejecting = station(json!({"security_log": true, "reject_get_log": true}));
    assert_eq!(
        rejecting.handle_call("GetLog", &get_log("SecurityLog", 1, json!({}))),
        json!({"status": "Rejected"})
    );
    assert_eq!(rejecting.snapshot()["active"], false);

    let legacy_only = station(json!({}));
    assert_eq!(
        legacy_only.handle_call("GetLog", &get_log("SecurityLog", 1, json!({}))),
        json!({"callError": "NotImplemented"})
    );
    let log_only = station(json!({"legacy": false, "security_log": true}));
    assert_eq!(
        log_only.handle_call("GetDiagnostics", &get_diagnostics(json!({}))),
        json!({"callError": "NotImplemented"})
    );
    assert_eq!(log_only.snapshot()["active"], false);
}

#[test]
fn strict_validation_refuses_malformed_requests_without_starting_an_upload() {
    let station = station(json!({"security_log": true}));
    let formation = [
        ("GetDiagnostics", get_diagnostics(json!({"unexpected": 1}))),
        ("GetDiagnostics", get_diagnostics(json!({"retries": null}))),
        ("GetDiagnostics", json!({})),
        ("GetDiagnostics", get_diagnostics(json!({"retries": "1"}))),
        (
            "GetLog",
            get_log("SecurityLog", 1, json!({"retryInterval": null})),
        ),
        ("GetLog", json!({"logType": "SecurityLog", "requestId": 1})),
        (
            "GetLog",
            get_log(
                "SecurityLog",
                1,
                json!({"log": {"remoteLocation": "http://x/", "extra": true}}),
            ),
        ),
    ];
    for (action, payload) in formation {
        assert_eq!(
            station.handle_call(action, &payload),
            json!({"callError": "FormationViolation"}),
            "{payload}"
        );
    }
    let long = format!("http://example.test/{}", "a".repeat(500));
    let property = [
        ("GetDiagnostics", get_diagnostics(json!({"retries": -1}))),
        (
            "GetDiagnostics",
            get_diagnostics(json!({"retryInterval": 2_147_483_648_i64})),
        ),
        ("GetDiagnostics", get_diagnostics(json!({"location": ""}))),
        (
            "GetDiagnostics",
            get_diagnostics(json!({"location": "not a uri"})),
        ),
        (
            "GetDiagnostics",
            get_diagnostics(json!({"startTime": "yesterday"})),
        ),
        (
            "GetDiagnostics",
            get_diagnostics(json!({
                "startTime": "2026-10-07T12:00:00Z", "stopTime": "2026-10-07T11:00:00Z"
            })),
        ),
        ("GetLog", get_log("AuditLog", 1, json!({}))),
        ("GetLog", get_log("SecurityLog", 2_147_483_648, json!({}))),
        (
            "GetLog",
            get_log("SecurityLog", 1, json!({"log": {"remoteLocation": long}})),
        ),
        (
            "GetLog",
            get_log(
                "SecurityLog",
                1,
                json!({"log": {"remoteLocation": "http://x/", "oldestTimestamp": "2026-13-01T00:00:00Z"}}),
            ),
        ),
    ];
    for (action, payload) in property {
        assert_eq!(
            station.handle_call(action, &payload),
            json!({"callError": "PropertyConstraintViolation"}),
            "{payload}"
        );
    }
    assert_eq!(station.snapshot()["active"], false);
    assert!(
        station
            .advance(OffsetDateTime::now_utc())
            .unwrap()
            .is_none()
    );
}

#[test]
fn undelivered_statuses_stay_queued_in_order() {
    let station = station(json!({}));
    started(&station, "GetDiagnostics", &get_diagnostics(json!({})));
    upload(&station, true);
    let now = OffsetDateTime::now_utc();
    let first = station.next_status(now).unwrap();
    assert!(station.next_status(now).is_none(), "one CALL in flight");
    station.status_finished(&first, false, now).unwrap();
    assert!(station.next_status(now).is_none(), "retry is delayed");
    let later = now + time::Duration::seconds(2);
    let retried = station.next_status(later).unwrap();
    assert_eq!(retried, first);
    station.status_finished(&retried, true, later).unwrap();
    assert_eq!(names(&drain(&station)), ["Uploaded"]);
}

#[test]
fn configuration_bounds_are_validated() {
    for fields in [
        json!({"legacy": false}),
        json!({"maximum_bytes": 0}),
        json!({"maximum_bytes": 67_108_865}),
        json!({"upload_timeout_ms": 0}),
        json!({"upload_timeout_ms": 300_001}),
        json!({"status_delay_ms": 30_001}),
        json!({"upload_failures": 17}),
        json!({"diagnostics_bytes": 0}),
        json!({"maximum_bytes": 100, "diagnostics_bytes": 101}),
        json!({"security_log": true, "security_log_bytes": 0}),
        json!({"legacy": false, "security_log": true, "no_diagnostics": true}),
        json!({"reject_get_log": true}),
        json!({"get_log_failure_status": "BadMessage"}),
    ] {
        assert!(
            DiagnosticsHandle::in_memory("alpha", &config(fields.clone())).is_err(),
            "{fields}"
        );
    }
    assert!(
        serde_json::from_value::<uob_sim::diagnostics16::DiagnosticsConfig>(json!({
            "private_state_file": "/x", "cancel_policy": "ignore"
        }))
        .is_err()
    );
}
