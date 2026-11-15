//! Native security station semantics: Security Whitepaper Ed4 A04 (durable guaranteed delivery,
//! schema bounds) and the status rules behind `ExtendedTriggerMessage` (N01.FR.12, L01.FR.28/29).
mod diagnostics16_support;
mod firmware16_support;
mod security16_support;
use firmware16_support::TestPki;
use firmware16_support::model::{
    IMAGE, download, drain as drain_firmware, signed, signed_update, up,
};
use security16_support::{config, drain, now, private_directory, station};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uob_sim::security16::{Delivery, SecurityHandle};

#[test]
fn events_are_claimed_one_at_a_time_in_order_and_leave_only_when_acknowledged() {
    let handle = station(json!({}));
    for kind in ["StartupOfTheDevice", "ResetOrReboot"] {
        handle
            .emit(kind, Some("2026-10-01T12:00:00Z"), None, now())
            .unwrap();
    }
    let first = handle.next_event(now()).unwrap();
    assert_eq!(
        first.payload(),
        json!({"type": "StartupOfTheDevice", "timestamp": "2026-10-01T12:00:00Z"})
    );
    // One CALL in flight at a time.
    assert!(handle.next_event(now()).is_none());
    assert_eq!(handle.snapshot()["pendingEvents"], 2);
    handle
        .event_finished(&first, Delivery::Acknowledged, now())
        .unwrap();
    let second = handle.next_event(now()).unwrap();
    assert_eq!(second.kind, "ResetOrReboot");
    handle
        .event_finished(&second, Delivery::Acknowledged, now())
        .unwrap();
    let snapshot = handle.snapshot();
    assert_eq!(
        (
            &snapshot["pendingEvents"],
            &snapshot["eventsEmitted"],
            &snapshot["eventsAcknowledged"],
            &snapshot["lastAcknowledgedType"]
        ),
        (&json!(0), &json!(2), &json!(2), &json!("ResetOrReboot"))
    );
}

#[test]
fn an_unanswered_event_stays_queued_and_is_retried_after_a_second() {
    let handle = station(json!({}));
    handle
        .emit("InvalidMessages", None, Some("bad frame"), now())
        .unwrap();
    let event = handle.next_event(now()).unwrap();
    let failed_at = OffsetDateTime::now_utc();
    handle
        .event_finished(&event, Delivery::Failed, failed_at)
        .unwrap();
    assert!(handle.next_event(failed_at).is_none(), "backs off");
    let retried = handle
        .next_event(failed_at + Duration::seconds(2))
        .expect("resent");
    assert_eq!(retried, event, "same identity and timestamp are resent");
    assert_eq!(retried.payload()["techInfo"], "bad frame");
    assert_eq!(handle.snapshot()["eventsAcknowledged"], 0);
}

#[test]
fn a_final_callerror_drops_the_event_and_counts_it() {
    let handle = station(json!({}));
    handle.emit("InvalidMessages", None, None, now()).unwrap();
    let event = handle.next_event(now()).unwrap();
    handle
        .event_finished(&event, Delivery::Rejected, now())
        .unwrap();
    let snapshot = handle.snapshot();
    assert_eq!(
        (&snapshot["pendingEvents"], &snapshot["eventsRejected"]),
        (&json!(0), &json!(1))
    );
}

#[test]
fn duplicate_fault_resends_an_identical_acknowledged_event_once() {
    let handle = station(json!({"duplicate_acknowledged": true}));
    handle
        .emit(
            "TamperDetectionActivated",
            Some("2026-10-01T12:00:00Z"),
            None,
            now(),
        )
        .unwrap();
    let sent = drain(&handle);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "identical type, timestamp and techInfo");
    let snapshot = handle.snapshot();
    assert_eq!(
        (
            &snapshot["pendingEvents"],
            &snapshot["duplicatesSent"],
            &snapshot["eventsEmitted"]
        ),
        (&json!(0), &json!(1), &json!(1))
    );
}

#[test]
fn schema_bounds_fail_locally_with_a_clear_code_and_queue_nothing() {
    let handle = station(json!({}));
    let at = Some("2026-10-01T12:00:00Z");
    let refused = [
        (
            handle.emit(&"t".repeat(51), at, None, now()),
            "security_event_type_too_long",
        ),
        (
            handle.emit("x", at, Some(&"i".repeat(256)), now()),
            "security_event_tech_info_too_long",
        ),
        (
            handle.emit("x", Some("yesterday"), None, now()),
            "security_event_timestamp_invalid",
        ),
        (
            handle.emit("x", Some("2026-10-01 12:00:00"), None, now()),
            "security_event_timestamp_invalid",
        ),
    ];
    for (result, code) in refused {
        assert_eq!(result.unwrap_err(), code);
    }
    assert_eq!(handle.snapshot()["pendingEvents"], 0);
    // Exactly at the bounds, counted in characters rather than bytes.
    handle
        .emit(&"é".repeat(50), at, Some(&"é".repeat(255)), now())
        .unwrap();
    assert_eq!(handle.snapshot()["pendingEvents"], 1);
}

#[test]
fn an_omitted_timestamp_is_the_current_time_with_millisecond_precision() {
    let handle = station(json!({}));
    let at =
        OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap() + Duration::milliseconds(123);
    handle.emit("SettingSystemTime", None, None, at).unwrap();
    let payload = handle.next_event(at).unwrap().payload();
    assert_eq!(payload["timestamp"], "2026-09-21T14:13:20.123Z");
}

#[test]
fn queued_events_survive_a_restart_with_their_order_and_unacknowledged_state() {
    let directory = private_directory("security-model");
    let file = directory.join("security.json");
    let options = config(json!({"private_state_file": file}));
    let handle = SecurityHandle::open("alpha", &[1], &options).unwrap();
    for kind in ["StartupOfTheDevice", "ResetOrReboot", "InvalidMessages"] {
        handle
            .emit(kind, Some("2026-10-01T12:00:00Z"), None, now())
            .unwrap();
    }
    // The first event was sent but never acknowledged when the process died.
    let in_flight = handle.next_event(now()).unwrap();
    // The lock keeps a second owner out until the first one is gone.
    assert!(SecurityHandle::open("alpha", &[1], &options).is_err());
    drop(handle);
    let restarted = SecurityHandle::open("alpha", &[1], &options).unwrap();
    assert_eq!(restarted.snapshot()["pendingEvents"], 3);
    let resent = restarted.next_event(now()).unwrap();
    assert_eq!(resent, in_flight);
    restarted
        .event_finished(&resent, Delivery::Acknowledged, now())
        .unwrap();
    drop(restarted);
    let again = SecurityHandle::open("alpha", &[1], &options).unwrap();
    let kinds: Vec<_> = drain(&again)
        .iter()
        .map(|event| event["type"].clone())
        .collect();
    assert_eq!(kinds, [json!("ResetOrReboot"), json!("InvalidMessages")]);
    drop(again);
    assert_eq!(
        SecurityHandle::open("beta", &[1], &options).unwrap_err(),
        "security_state_invalid"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configuration_bounds_and_unknown_keys_are_refused() {
    assert!(
        SecurityHandle::in_memory(
            "alpha",
            &[1],
            &config(json!({"extended_trigger": {"reply_delay_ms": 30001}}))
        )
        .is_err()
    );
    assert!(
        SecurityHandle::in_memory("alpha", &[1], &config(json!({"private_state_file": ""})))
            .is_err()
    );
    assert!(
        serde_json::from_value::<uob_sim::security16::SecurityConfig>(
            json!({"private_state_file": "/x", "unknown": 1})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<uob_sim::security16::SecurityConfig>(
            json!({"private_state_file": "/x", "extended_trigger": {"heartbeat": "maybe"}})
        )
        .is_err()
    );
}

#[test]
fn triggered_log_status_is_idle_without_request_id_unless_a_get_log_upload_is_ongoing() {
    let log = diagnostics16_support::station(json!({"security_log": true}));
    assert_eq!(log.extended_trigger_status(), ("Idle", None));
    let request = diagnostics16_support::get_log("SecurityLog", 5, json!({}));
    assert_eq!(log.handle_call("GetLog", &request)["status"], "Accepted");
    assert_eq!(log.extended_trigger_status(), ("Uploading", Some(5)));
    diagnostics16_support::upload(&log, true);
    assert_eq!(log.extended_trigger_status(), ("Idle", None));
    // A legacy GetDiagnostics upload is not a log upload.
    let legacy = diagnostics16_support::station(json!({"security_log": true}));
    let request = diagnostics16_support::get_diagnostics(json!({}));
    let _ = legacy.handle_call("GetDiagnostics", &request);
    assert_eq!(legacy.extended_trigger_status(), ("Idle", None));
}

#[test]
fn triggered_signed_firmware_status_is_idle_after_installed_else_the_last_sent_status() {
    let pki = TestPki::generate();
    let handle = signed(&pki, json!({}));
    assert_eq!(handle.extended_trigger_status(), ("Idle".to_owned(), None));
    let at = OffsetDateTime::now_utc();
    let _ = handle.handle_call(
        "SignedUpdateFirmware",
        &signed_update(&pki, 77, at, None),
        at,
    );
    download(&handle, up(1), Some(IMAGE));
    drain_firmware(&handle);
    // L01.FR.29: last sent status other than Installed, with the update's requestId.
    assert_eq!(
        handle.extended_trigger_status(),
        ("InstallRebooting".to_owned(), Some(77))
    );
    handle.advance(at, up(1)).unwrap();
    handle.advance(at, up(2)).unwrap();
    drain_firmware(&handle);
    // L01.FR.28: last sent status Installed.
    assert_eq!(handle.extended_trigger_status(), ("Idle".to_owned(), None));
}
