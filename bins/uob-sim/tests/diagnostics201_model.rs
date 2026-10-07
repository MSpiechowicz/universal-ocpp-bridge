//! Native log-upload semantics of the in-memory 2.0.1 station model (OCPP 2.0.1 N01).
mod diagnostics201_support;
use diagnostics201_support::{attempt, config, drain, get_log, names, station, upload};
use serde_json::json;
use time::OffsetDateTime;
use uob_sim::diagnostics201::Diagnostics201Handle;

/// Sends a request that must start an upload job.
fn started(station: &Diagnostics201Handle, payload: &serde_json::Value) {
    let reply = station.handle_call(payload);
    assert!(reply.get("filename").is_some(), "{reply}");
}

#[test]
fn get_log_names_a_file_uploads_once_and_reports_each_status_with_its_request_id() {
    let station = station(json!({}));
    assert_eq!(station.trigger_payload(), json!({"status": "Idle"}));
    let reply = station.handle_call(&get_log("SecurityLog", 7, json!({})));
    assert_eq!(
        reply,
        json!({"status": "Accepted", "filename": "securitylog-alpha-1.log"})
    );
    let ticket = attempt(&station);
    assert_eq!(ticket.file_name, "securitylog-alpha-1.log");
    assert!(ticket.content().starts_with(b"UOB-SIM SECURITY LOG\n"));
    assert_eq!(ticket.content().len(), 2048);
    // N01.FR.13: a triggered status while uploading names the ongoing request.
    assert_eq!(
        station.trigger_payload(),
        json!({"status": "Uploading", "requestId": 7})
    );
    station
        .upload_finished(&ticket, true, OffsetDateTime::now_utc())
        .unwrap();
    // N01.FR.07/08/09: every status carries the same requestId.
    assert_eq!(
        drain(&station),
        [("Uploading".to_owned(), 7), ("Uploaded".to_owned(), 7)]
    );
    assert_eq!(station.trigger_payload(), json!({"status": "Idle"}));
    let snapshot = station.snapshot();
    assert_eq!(snapshot["active"], false);
    assert_eq!(snapshot["uploads"], 1);
    assert_eq!(snapshot["requestId"], 7);
    assert_eq!(snapshot["statuses"], json!(["Uploading", "Uploaded"]));
    assert!(!snapshot.to_string().contains("127.0.0.1"));
    assert!(!format!("{ticket:?}").contains("127.0.0.1"));

    // N01.FR.03/04: the log type selects the generated content.
    started(&station, &get_log("DiagnosticsLog", 8, json!({})));
    let ticket = attempt(&station);
    assert_eq!(ticket.file_name, "diagnosticslog-alpha-2.log");
    assert!(ticket.content().starts_with(b"UOB-SIM DIAGNOSTICS LOG\n"));
    assert_eq!(ticket.content().len(), 4096);
    assert_eq!(station.snapshot()["kind"], "DiagnosticsLog");
}

#[test]
fn requested_window_is_carried_into_the_generated_log_header() {
    let station = station(json!({}));
    started(
        &station,
        &get_log(
            "DiagnosticsLog",
            1,
            json!({"log": {
                "remoteLocation": "http://127.0.0.1:9/uploads/log/",
                "oldestTimestamp": "2026-10-06T00:00:00Z",
                "latestTimestamp": "2026-10-07T00:00:00Z",
            }}),
        ),
    );
    let ticket = attempt(&station);
    let text = String::from_utf8_lossy(ticket.content());
    assert!(text.contains("from=2026-10-06T00:00:00Z\n"));
    assert!(text.contains("to=2026-10-07T00:00:00Z\n"));
}

#[test]
fn bounded_retries_report_uploading_per_attempt_then_a_single_end_state() {
    let recovered = station(json!({"upload_failures": 2}));
    started(
        &recovered,
        &get_log(
            "DiagnosticsLog",
            4,
            json!({"retries": 2, "retryInterval": 0}),
        ),
    );
    assert!(upload(&recovered, true).simulated_failure);
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
        &get_log("SecurityLog", 5, json!({"retries": 1, "retryInterval": 0})),
    );
    upload(&failed, false);
    upload(&failed, false);
    assert_eq!(
        names(&drain(&failed)),
        ["Uploading", "Uploading", "UploadFailure"]
    );
    assert_eq!(failed.snapshot()["uploads"], 0);
}

#[test]
fn retry_interval_delays_the_next_attempt() {
    let station = station(json!({}));
    started(
        &station,
        &get_log("SecurityLog", 1, json!({"retries": 1, "retryInterval": 30})),
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
fn failure_uses_the_configured_n01_fr_10_status_after_every_attempt() {
    for status in [
        "UploadFailure",
        "BadMessage",
        "PermissionDenied",
        "NotSupportedOperation",
    ] {
        let station = station(json!({"upload_failures": 2, "failure_status": status}));
        started(
            &station,
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
                ("Uploading".to_owned(), 3),
                ("Uploading".to_owned(), 3),
                (status.to_owned(), 3)
            ],
            "{status}"
        );
    }
}

#[test]
fn new_get_log_cancels_with_its_own_notification_or_is_rejected_during_an_ongoing_upload() {
    let cancelling = station(json!({}));
    started(&cancelling, &get_log("SecurityLog", 1, json!({})));
    let stale = attempt(&cancelling);
    // N01.FR.12: cancel the ongoing upload and answer AcceptedCanceled.
    assert_eq!(
        cancelling.handle_call(&get_log("DiagnosticsLog", 2, json!({}))),
        json!({"status": "AcceptedCanceled", "filename": "diagnosticslog-alpha-2.log"})
    );
    // The cancelled upload sends no further progress, even when its transfer later completes.
    cancelling
        .upload_finished(&stale, true, OffsetDateTime::now_utc())
        .unwrap();
    upload(&cancelling, true);
    // N01.FR.20: AcceptedCanceled carries the cancelled upload's requestId (N01.FR.07).
    assert_eq!(
        drain(&cancelling),
        [
            ("AcceptedCanceled".to_owned(), 1),
            ("Uploading".to_owned(), 2),
            ("Uploaded".to_owned(), 2)
        ]
    );
    let snapshot = cancelling.snapshot();
    assert_eq!(snapshot["cancelled"], json!([1]));
    assert_eq!(snapshot["uploads"], 1);
    assert_eq!(snapshot["requestId"], 2);

    let rejecting = station(json!({"cancel_policy": "reject"}));
    started(&rejecting, &get_log("SecurityLog", 1, json!({})));
    assert_eq!(
        rejecting.handle_call(&get_log("SecurityLog", 2, json!({}))),
        json!({"status": "Rejected", "statusInfo": {"reasonCode": "UnableToCancel"}})
    );
    assert_eq!(rejecting.snapshot()["requestId"], 1);
    assert_eq!(rejecting.snapshot()["cancelled"], json!([]));
}

#[test]
fn a_finished_upload_is_not_cancelled_by_the_next_request() {
    let station = station(json!({}));
    started(&station, &get_log("SecurityLog", 1, json!({})));
    upload(&station, true);
    assert_eq!(
        station.handle_call(&get_log("SecurityLog", 2, json!({})))["status"],
        "Accepted"
    );
    assert_eq!(station.snapshot()["cancelled"], json!([]));
}

#[test]
fn rejecting_station_starts_nothing_and_explains_why() {
    let rejecting = station(json!({"reject_get_log": true}));
    // N01.FR.05: the requested log information is not available.
    assert_eq!(
        rejecting.handle_call(&get_log("SecurityLog", 1, json!({}))),
        json!({"status": "Rejected", "statusInfo": {"reasonCode": "NoLogAvailable"}})
    );
    assert_eq!(rejecting.snapshot()["active"], false);
    assert!(
        rejecting
            .advance(OffsetDateTime::now_utc())
            .unwrap()
            .is_none()
    );
}

#[test]
fn strict_validation_refuses_malformed_requests_without_starting_an_upload() {
    let station = station(json!({}));
    let formation = [
        get_log("SecurityLog", 1, json!({"unexpected": 1})),
        get_log("SecurityLog", 1, json!({"retries": null})),
        get_log("SecurityLog", 1, json!({"retryInterval": null})),
        get_log("SecurityLog", 1, json!({"retries": "1"})),
        json!({"logType": "SecurityLog", "requestId": 1}),
        json!({"log": {"remoteLocation": "http://x/"}, "requestId": 1}),
        json!({"log": {"remoteLocation": "http://x/"}, "logType": "SecurityLog"}),
        get_log(
            "SecurityLog",
            1,
            json!({"log": {"remoteLocation": "http://x/", "extra": true}}),
        ),
        get_log("SecurityLog", 1, json!({"log": {}})),
        get_log("SecurityLog", 1, json!({"log": {"remoteLocation": null}})),
        get_log("SecurityLog", 1, json!({"customData": {}})),
        // Legacy 1.6 field names are not 2.0.1 fields.
        json!({"location": "http://x/", "logType": "SecurityLog", "requestId": 1}),
        json!({}),
        json!([]),
    ];
    for payload in formation {
        assert_eq!(
            station.handle_call(&payload),
            json!({"callError": "FormationViolation"}),
            "{payload}"
        );
    }
    let long = format!("http://example.test/{}", "a".repeat(500));
    let property = [
        get_log("SecurityLog", 1, json!({"retries": -1})),
        get_log(
            "SecurityLog",
            1,
            json!({"retryInterval": 2_147_483_648_i64}),
        ),
        get_log("SecurityLog", 1, json!({"log": {"remoteLocation": ""}})),
        get_log(
            "SecurityLog",
            1,
            json!({"log": {"remoteLocation": "not a uri"}}),
        ),
        get_log("AuditLog", 1, json!({})),
        get_log("SecurityLog", 2_147_483_648, json!({})),
        get_log("SecurityLog", 1, json!({"log": {"remoteLocation": long}})),
        get_log(
            "SecurityLog",
            1,
            json!({"log": {"remoteLocation": "http://x/", "oldestTimestamp": "2026-13-01T00:00:00Z"}}),
        ),
        get_log(
            "SecurityLog",
            1,
            json!({"log": {
                "remoteLocation": "http://x/",
                "oldestTimestamp": "2026-10-07T12:00:00Z",
                "latestTimestamp": "2026-10-07T11:00:00Z",
            }}),
        ),
    ];
    for payload in property {
        let reply = station.handle_call(&payload);
        assert!(
            matches!(
                reply["callError"].as_str(),
                Some("PropertyConstraintViolation" | "FormationViolation")
            ),
            "{payload}: {reply}"
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
    started(&station, &get_log("SecurityLog", 1, json!({})));
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
fn trigger_payload_reports_the_ongoing_request_until_it_finishes_or_is_cancelled() {
    let station = station(json!({}));
    started(&station, &get_log("DiagnosticsLog", 12, json!({})));
    assert_eq!(
        station.trigger_payload(),
        json!({"status": "Uploading", "requestId": 12})
    );
    started(&station, &get_log("DiagnosticsLog", 13, json!({})));
    assert_eq!(
        station.trigger_payload(),
        json!({"status": "Uploading", "requestId": 13})
    );
    upload(&station, false);
    assert_eq!(station.trigger_payload(), json!({"status": "Idle"}));
}

#[test]
fn configuration_bounds_are_validated() {
    for fields in [
        json!({"maximum_bytes": 0}),
        json!({"maximum_bytes": 67_108_865}),
        json!({"upload_timeout_ms": 0}),
        json!({"upload_timeout_ms": 300_001}),
        json!({"status_delay_ms": 30_001}),
        json!({"upload_failures": 17}),
        json!({"diagnostics_bytes": 0}),
        json!({"security_log_bytes": 0}),
        json!({"maximum_bytes": 100, "diagnostics_bytes": 101}),
        json!({"reject_get_log": true, "upload_failures": 1}),
    ] {
        assert!(
            Diagnostics201Handle::in_memory("alpha", &config(fields.clone())).is_err(),
            "{fields}"
        );
    }
    for fields in [
        json!({"cancel_policy": "ignore"}),
        json!({"failure_status": "Idle"}),
        json!({"failure_status": "UploadFailed"}),
        json!({"legacy": true}),
    ] {
        let mut base = json!({"private_state_file": "/x"});
        base.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(
            serde_json::from_value::<uob_sim::diagnostics201::Diagnostics201Config>(base).is_err(),
            "{fields}"
        );
    }
}
