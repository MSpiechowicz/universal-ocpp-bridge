//! The diagnostics station model is derived from the pinned OCA sources, never from bridge code:
//! no bridge crate is reachable, and every emitted value is checked against the pinned schema
//! or the hand-transcribed Security Whitepaper Ed4 enumerations.
mod diagnostics16_support;
use diagnostics16_support::{drain, get_diagnostics, get_log, station, upload};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Whitepaper Ed4 `UploadLogStatusEnumType` (LogStatusNotification.req).
const UPLOAD_LOG_STATUS: &[&str] = &[
    "BadMessage",
    "Idle",
    "NotSupportedOperation",
    "PermissionDenied",
    "Uploaded",
    "UploadFailure",
    "Uploading",
];
/// Whitepaper Ed4 `LogStatusEnumType` (GetLog.conf).
const GET_LOG_STATUS: &[&str] = &["Accepted", "Rejected", "AcceptedCanceled"];

fn pinned_legacy_statuses() -> BTreeSet<String> {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../tests/ocpp-fixtures/corpus/schemas/1.6/DiagnosticsStatusNotification.json"
    ))
    .unwrap();
    schema["properties"]["status"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn simulator_reaches_no_bridge_crate() {
    let manifest = include_str!("../Cargo.toml");
    let bridge: Vec<_> = manifest
        .lines()
        .filter(|line| line.starts_with("uob"))
        .collect();
    assert!(
        bridge.is_empty(),
        "simulator depends on a bridge crate: {bridge:?}"
    );
    for source in [
        include_str!("../src/diagnostics16.rs"),
        include_str!("../src/diagnostics16/config.rs"),
        include_str!("../src/diagnostics16/model.rs"),
        include_str!("../src/diagnostics16/request.rs"),
        include_str!("../src/diagnostics16/transport.rs"),
        include_str!("../src/scenario/diagnostics16.rs"),
    ] {
        assert!(!source.contains("uob_"), "bridge path in simulator source");
    }
}

#[test]
fn every_emitted_legacy_status_is_in_the_pinned_schema_enum() {
    let pinned = pinned_legacy_statuses();
    let mut emitted = BTreeSet::new();
    for (failures, retries) in [(0, 0), (1, 0), (1, 1)] {
        let station = station(json!({"upload_failures": failures}));
        assert!(
            station
                .handle_call(
                    "GetDiagnostics",
                    &get_diagnostics(json!({"retries": retries, "retryInterval": 0})),
                )
                .get("fileName")
                .is_some()
        );
        for _ in 0..=retries {
            upload(&station, true);
        }
        emitted.extend(drain(&station).into_iter().map(|(status, id)| {
            assert!(id.is_none(), "legacy statuses carry no requestId");
            status
        }));
        emitted.insert(station.trigger_status().to_owned());
    }
    assert!(emitted.is_subset(&pinned), "{emitted:?} vs {pinned:?}");
    assert_eq!(emitted, pinned, "every pinned status is exercised");
}

#[test]
fn every_get_log_reply_and_status_is_in_the_whitepaper_enumerations() {
    let mut replies = BTreeSet::new();
    let mut statuses = BTreeSet::new();
    for failure in [
        "UploadFailure",
        "BadMessage",
        "PermissionDenied",
        "NotSupportedOperation",
    ] {
        let station = station(json!({
            "security_log": true, "upload_failures": 1, "get_log_failure_status": failure
        }));
        for id in [1, 2] {
            let reply = station.handle_call("GetLog", &get_log("SecurityLog", id, json!({})));
            let filename = reply["filename"].as_str().unwrap();
            assert!(filename.len() <= 255 && filename.is_ascii());
            replies.insert(reply["status"].as_str().unwrap().to_owned());
        }
        upload(&station, true);
        statuses.extend(drain(&station).into_iter().map(|(status, id)| {
            assert_eq!(id, Some(2), "N01.FR.07");
            status
        }));
    }
    let rejecting = station(json!({"security_log": true, "reject_get_log": true}));
    let reply = rejecting.handle_call("GetLog", &get_log("DiagnosticsLog", 1, json!({})));
    replies.insert(reply["status"].as_str().unwrap().to_owned());
    let success = station(json!({"security_log": true}));
    let _ = success.handle_call("GetLog", &get_log("DiagnosticsLog", 3, json!({})));
    upload(&success, true);
    statuses.extend(drain(&success).into_iter().map(|(status, _)| status));
    assert!(
        replies
            .iter()
            .all(|status| GET_LOG_STATUS.contains(&status.as_str()))
    );
    assert_eq!(replies.len(), GET_LOG_STATUS.len());
    assert!(
        statuses
            .iter()
            .all(|status| UPLOAD_LOG_STATUS.contains(&status.as_str())),
        "{statuses:?}"
    );
}
