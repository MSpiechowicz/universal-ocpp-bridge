//! The 2.0.1 log station model is derived from the pinned OCA sources and the independently
//! authored corpus, never from bridge code: no bridge crate is reachable, every emitted value is
//! checked against the pinned schema enumerations and the corpus frames.
mod diagnostics201_support;
use diagnostics201_support::{drain, get_log, station, upload};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn wire(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/ocpp-fixtures/corpus/wire/2.0.1/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn enumeration(schema: &str, definition: &str) -> BTreeSet<String> {
    let schema: Value = serde_json::from_str(schema).unwrap();
    schema["definitions"][definition]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

fn pinned_upload_statuses() -> BTreeSet<String> {
    enumeration(
        include_str!(
            "../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/LogStatusNotificationRequest.json"
        ),
        "UploadLogStatusEnumType",
    )
}

fn pinned_reply_statuses() -> BTreeSet<String> {
    enumeration(
        include_str!("../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetLogResponse.json"),
        "LogStatusEnumType",
    )
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
        include_str!("../src/diagnostics201.rs"),
        include_str!("../src/diagnostics201/config.rs"),
        include_str!("../src/diagnostics201/model.rs"),
        include_str!("../src/diagnostics201/request.rs"),
        include_str!("../src/diagnostics201/transport.rs"),
    ] {
        assert!(!source.contains("uob_"), "bridge path in simulator source");
    }
}

#[test]
fn corpus_requests_drive_the_real_station_model_to_the_corpus_replies() {
    for (request, kind) in [
        ("log-get-diagnostics-log", "DiagnosticsLog"),
        ("log-get-diagnostics-log-minimal", "DiagnosticsLog"),
        ("log-get-security-log", "SecurityLog"),
        ("log-get-security-log-minimal", "SecurityLog"),
    ] {
        let frame = wire(request);
        let station = station(json!({}));
        let reply = station.handle_call(&frame[3]);
        assert_eq!(reply["status"], "Accepted", "{request}");
        // The reply has exactly the corpus shape: a status and a CiString255 file name.
        let corpus = wire("log-get-reply-accepted")[2].clone();
        assert_eq!(
            reply.as_object().unwrap().keys().collect::<Vec<_>>(),
            corpus.as_object().unwrap().keys().collect::<Vec<_>>(),
            "{request}"
        );
        let filename = reply["filename"].as_str().unwrap();
        assert!(filename.len() <= 255 && filename.is_ascii());
        let snapshot = station.snapshot();
        assert_eq!(snapshot["kind"], kind, "{request}");
        assert_eq!(snapshot["requestId"], frame[3]["requestId"], "{request}");
    }
    // N01.FR.12: a second corpus request cancels the first.
    let station = station(json!({}));
    let _ = station.handle_call(&wire("log-get-security-log")[3]);
    let reply = station.handle_call(&wire("log-get-diagnostics-log")[3]);
    assert_eq!(
        reply["status"],
        wire("log-get-reply-accepted-canceled")[2]["status"]
    );
    // N01.FR.05: a station without logs answers like the corpus Rejected reply.
    let empty = self::station(json!({"reject_get_log": true}));
    assert_eq!(
        empty.handle_call(&wire("log-get-security-log")[3]),
        wire("log-get-reply-rejected-status-info")[2]
            .as_object()
            .map(|reply| json!({"status": reply["status"], "statusInfo": {"reasonCode": reply["statusInfo"]["reasonCode"]}}))
            .unwrap()
    );
}

#[test]
fn corpus_negative_requests_never_start_an_upload() {
    let cases = wire("log-negative-cases");
    let mut refused = 0;
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][2] != "GetLog" {
            continue;
        }
        let station = station(json!({}));
        let reply = station.handle_call(&case["wire"][3]);
        assert!(
            matches!(
                reply["callError"].as_str(),
                Some("FormationViolation" | "PropertyConstraintViolation")
            ),
            "{}: {reply}",
            case["id"]
        );
        assert_eq!(station.snapshot()["active"], false, "{}", case["id"]);
        refused += 1;
    }
    assert!(refused >= 8, "{refused}");
}

#[test]
fn every_emitted_status_and_reply_is_in_the_pinned_enumerations() {
    let pinned = pinned_upload_statuses();
    let mut statuses = BTreeSet::new();
    let mut replies = BTreeSet::new();
    for failure in [
        "UploadFailure",
        "BadMessage",
        "PermissionDenied",
        "NotSupportedOperation",
    ] {
        let station = station(json!({"upload_failures": 1, "failure_status": failure}));
        for id in [1, 2] {
            let reply = station.handle_call(&get_log("SecurityLog", id, json!({})));
            replies.insert(reply["status"].as_str().unwrap().to_owned());
        }
        upload(&station, true);
        statuses.extend(drain(&station).into_iter().map(|(status, id)| {
            assert!(id == 1 || id == 2, "N01.FR.07");
            status
        }));
    }
    let rejecting = station(json!({"reject_get_log": true}));
    let reply = rejecting.handle_call(&get_log("DiagnosticsLog", 1, json!({})));
    replies.insert(reply["status"].as_str().unwrap().to_owned());
    let success = station(json!({}));
    let _ = success.handle_call(&get_log("DiagnosticsLog", 3, json!({})));
    upload(&success, true);
    statuses.extend(drain(&success).into_iter().map(|(status, _)| status));
    // Idle is reported only by a trigger, without a requestId (N01.FR.13).
    statuses.insert(
        success.trigger_payload()["status"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    assert!(statuses.is_subset(&pinned), "{statuses:?} vs {pinned:?}");
    assert_eq!(statuses, pinned, "every pinned status is exercised");
    assert_eq!(replies, pinned_reply_statuses());
}

#[test]
fn corpus_status_frames_match_what_the_station_reports() {
    // Every corpus status the station can originate has the corpus requestId shape.
    for name in [
        "uploading",
        "uploaded",
        "upload-failure",
        "bad-message",
        "permission-denied",
        "not-supported-operation",
        "accepted-canceled",
    ] {
        let frame = wire(&format!("log-status-{name}"));
        assert!(frame[3]["requestId"].is_i64(), "{name}");
        assert!(pinned_upload_statuses().contains(frame[3]["status"].as_str().unwrap()));
    }
    assert_eq!(
        wire("log-status-idle-unattributed")[3],
        json!({"status": "Idle"})
    );
    assert_eq!(
        station(json!({})).trigger_payload(),
        wire("log-status-idle-unattributed")[3]
    );
}
