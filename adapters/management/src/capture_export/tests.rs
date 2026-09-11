mod support;
use super::*;
use axum::{body::to_bytes, http::StatusCode};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::time::Duration;
use support::*;
use tower::ServiceExt;
use uob_application::capture::{CaptureError, CaptureLevel};

#[tokio::test]
async fn route_rechecks_permissions_and_exports_redacted_provenance() {
    let setup = Setup::new(4);
    setup.emit();
    for (token, status) in [
        ("bad", StatusCode::UNAUTHORIZED),
        ("control", StatusCode::FORBIDDEN),
        ("wrong-station", StatusCode::FORBIDDEN),
        ("wrong-target", StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            setup
                .router
                .clone()
                .oneshot(request(token, "process", setup.id))
                .await
                .unwrap()
                .status(),
            status
        );
    }
    for (process, id) in [("old", setup.id), ("process", setup.id + 1)] {
        assert_eq!(
            setup
                .router
                .clone()
                .oneshot(request("reader", process, id))
                .await
                .unwrap()
                .status(),
            StatusCode::GONE
        );
    }
    let mut duplicate = request("reader", "process", setup.id);
    duplicate
        .headers_mut()
        .append("authorization", "Bearer reader".parse().unwrap());
    assert_eq!(
        setup
            .router
            .clone()
            .oneshot(duplicate)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = setup.download().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/x-ndjson"
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("attachment")
    );
    let bytes = to_bytes(response.into_body(), 100_000).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    for secret in [
        "secret-token",
        "vendor-secret",
        "password=secret",
        "wrong-station",
    ] {
        assert!(!text.contains(secret));
    }
    let lines = lines(text);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["type"], "manifest");
    assert_eq!(lines[0]["schema_version"], "1.0");
    assert_eq!(lines[0]["build_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        lines[0]["identity"]["runtime"]["process_instance_id"],
        "process"
    );
    assert_eq!(lines[0]["filters"]["station_id"], "a");
    assert_eq!(lines[0]["filters"]["target_id"], "target");
    assert_eq!(
        lines[0]["configuration"]["capture_level"],
        "redacted_payload"
    );
    assert_eq!(lines[1]["record"]["redacted_details"]["truncated"], true);
    assert_eq!(lines[2]["truncated_records"], 1);
    assert_eq!(lines[2]["retained_window_complete"], true);
    assert_eq!(lines[2]["history_complete"], false);
    assert_eq!(lines[2]["exported_records"], 1);
    assert_eq!(
        usize::try_from(lines[2]["bytes_before_summary"].as_u64().unwrap()).unwrap(),
        text.lines().take(2).map(|v| v.len() + 1).sum::<usize>()
    );
    setup.stop();
    assert_eq!(setup.resources.snapshot().queues.capture_records, 0);
}

#[tokio::test]
async fn initial_overflow_and_producer_drops_are_explicit_and_cutoff_is_finite() {
    let setup = Setup::new(2);
    for _ in 0..5 {
        setup.emit();
    }
    assert!(!setup.manager.try_record(&filter(), |_, _| None));
    let response = setup.download().await;
    // These arrive after admission and must never be included, even if they evict the window.
    setup.emit();
    let lines = body(response).await;
    assert_eq!(lines[0]["window"]["evicted_records"], 3);
    assert_eq!(lines[0]["window"]["dropped_records"], 1);
    assert_eq!(lines[0]["window"]["next_sequence"], 6);
    assert_eq!(lines[1]["record"]["trace_sequence"], 4);
    let summary = lines.last().unwrap();
    assert_eq!(summary["reason"], "window_end");
    assert_eq!(summary["unexported_initial_records"], 1);
    assert_eq!(summary["missing_sequences"], 1);
    assert_eq!(summary["retained_window_complete"], false);
}

#[tokio::test]
async fn empty_and_fully_evicted_windows_end_without_following_live_traffic() {
    for populated in [false, true] {
        let setup = Setup::new(2);
        if populated {
            setup.emit();
        }
        let response = setup.download().await;
        for _ in 0..4 {
            setup.emit();
        }
        let lines = body(response).await;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["exported_records"], 0);
        assert_eq!(lines[1]["retained_window_complete"], !populated);
    }
}

#[tokio::test]
async fn scope_changes_revoke_download_before_the_next_record() {
    for mode in [1, 2, 3] {
        let setup = Setup::new(4);
        setup.emit();
        let mut stream = setup.download().await.into_body().into_data_stream();
        assert!(stream.next().await.unwrap().is_ok()); // manifest
        setup.auth.mode.store(mode, Ordering::SeqCst);
        let summary: Value =
            serde_json::from_slice(&stream.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(summary["reason"], "permission_revoked");
        assert_eq!(summary["exported_records"], 0);
        assert!(stream.next().await.is_none());
        setup.stop();
        assert_eq!(setup.resources.snapshot().queues.capture_records, 0);
    }
}

#[tokio::test]
async fn cancellation_completion_and_stop_release_bounded_export_slots() {
    let setup = Setup::new(4);
    setup.emit();
    let first = setup.download().await;
    let second = setup.download().await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        setup.download().await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(first);
    let third = setup.download().await;
    assert_eq!(third.status(), StatusCode::OK);
    let _ = body(third).await;
    assert!(
        setup
            .manager
            .lease(&grant("a", "target", true), setup.id, true)
            .is_ok()
    );
    setup.stop();
    tokio::time::sleep(Duration::from_millis(250)).await;
    // The unpolled second HTTP response holds no ring references after the watchdog runs.
    assert_eq!(setup.resources.snapshot().queues.capture_records, 0);
    assert!(
        setup
            .manager
            .start(
                &grant("a", "target", true),
                filter(),
                CaptureLevel::Metadata,
                None
            )
            .is_ok()
    );
    let lines = body(second).await;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["reason"], "capture_stopped_or_expired");
}

#[tokio::test]
async fn stalled_body_and_absolute_deadline_release_leases_without_body_polling() {
    for (lifetime, idle, reason) in [
        (
            Duration::from_secs(5),
            Duration::from_millis(40),
            "slow_reader",
        ),
        (Duration::from_millis(40), Duration::from_secs(5), "timeout"),
    ] {
        let setup = Setup::new(4);
        setup.emit();
        let response = setup.with_limits(stream::Limits {
            lifetime,
            idle,
            ..Default::default()
        });
        tokio::time::sleep(Duration::from_millis(250)).await;
        let lease_a = setup
            .manager
            .lease(&grant("a", "target", true), setup.id, true)
            .unwrap();
        let lease_b = setup
            .manager
            .lease(&grant("a", "target", true), setup.id, true)
            .unwrap();
        drop((lease_a, lease_b));
        setup.stop();
        assert_eq!(setup.resources.snapshot().queues.capture_records, 0);
        let lines = body(response).await;
        assert_eq!(lines[0]["reason"], reason);
        assert_eq!(lines[0]["exported_records"], 0);
    }
}

#[tokio::test]
async fn byte_and_record_limits_leave_an_explicit_incomplete_summary() {
    for (bytes, records, reason) in [
        (2 * format::MAX_METADATA_BYTES, 1, "record_limit"),
        (format::MAX_METADATA_BYTES + 1800, 10, "byte_limit"),
    ] {
        let setup = Setup::new(4);
        for _ in 0..4 {
            setup.emit();
        }
        let response = setup.with_limits(stream::Limits {
            bytes,
            records,
            ..Default::default()
        });
        let data = to_bytes(response.into_body(), bytes).await.unwrap();
        assert!(data.len() <= bytes);
        let lines = lines(std::str::from_utf8(&data).unwrap());
        let summary = lines.last().unwrap();
        assert_eq!(summary["reason"], reason);
        assert_eq!(summary["retained_window_complete"], false);
        setup.stop();
        assert_eq!(setup.resources.snapshot().queues.capture_records, 0);
    }
}

#[tokio::test]
async fn oversized_metadata_fails_bounded_and_does_not_consume_export_capacity() {
    let setup = Setup::new(4);
    let mut identity = identity();
    identity.runtime.release_id = uob_contracts::ReleaseId::new("\"".repeat(100_000)).unwrap();
    let router = crate::capture_router(identity, setup.configuration());
    let response = router
        .oneshot(request("reader", "process", setup.id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        setup
            .manager
            .lease(&grant("a", "target", true), setup.id, true)
            .is_ok()
    );
    setup.stop();
    assert_eq!(
        setup
            .manager
            .status(&grant("a", "target", true))
            .unwrap_err(),
        CaptureError::Gone
    );
}

#[tokio::test]
async fn capture_expiry_frees_memory_even_when_download_remains_unread() {
    let setup = Setup::new(4);
    setup.emit();
    let response = setup.download().await;
    setup
        .manager
        .extend(
            &grant("a", "target", true),
            setup.id,
            Duration::from_millis(40),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(setup.resources.snapshot().queues.capture_records, 0);
    assert!(setup.manager.wake_after(setup.id).is_none());
    assert_eq!(
        body(response).await[0]["reason"],
        "capture_stopped_or_expired"
    );
}

#[tokio::test]
async fn out_of_selection_records_never_reach_the_export_formatter() {
    let setup = Setup::new(4);
    let mut other_station = filter();
    other_station.station = Some(uob_contracts::StationId::new("wrong-station").unwrap());
    let mut other_target = filter();
    other_target.target = Some(uob_contracts::TargetInstanceId::new("wrong-target").unwrap());
    for selection in [other_station, other_target] {
        assert!(
            !setup
                .manager
                .try_record(&selection, |_, _| panic!("unauthorized record formatted"))
        );
    }
    let lines = body(setup.download().await).await;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["exported_records"], 0);
}
