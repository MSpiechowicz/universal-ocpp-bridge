use super::*;
use crate::{ManagementCaptureAuthenticator, ManagementCaptureConfiguration, capture_router};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
use uob_application::{
    CommandClock, FlowDiagnostics, FlowEvidence, FlowStage,
    capture::{CaptureFilter, CaptureGrant, CaptureLevel, CaptureManager, CapturePermission},
};
use uob_contracts::*;

struct Authentication;
impl ManagementCaptureAuthenticator for Authentication {
    fn authenticate(&self, token: &str) -> Option<CaptureGrant> {
        match token {
            "reader" => Some(grant("a", vec![CapturePermission::Read])),
            "wrong-station" => Some(grant("b", vec![CapturePermission::Read])),
            "control" => Some(grant("a", vec![CapturePermission::Capture])),
            _ => None,
        }
    }
}
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        serde_json::from_str("\"2026-09-11T00:00:00Z\"").unwrap()
    }
}
fn grant(station: &str, permissions: Vec<CapturePermission>) -> CaptureGrant {
    CaptureGrant::new(
        BridgeId::new("bridge").unwrap(),
        permissions,
        Some(vec![StationId::new(station).unwrap()]),
        None,
    )
    .unwrap()
}
fn filter() -> CaptureFilter {
    CaptureFilter {
        bridge: BridgeId::new("bridge").unwrap(),
        station: Some(StationId::new("a").unwrap()),
        target: None,
    }
}
fn identity() -> ServiceIdentity {
    ServiceIdentity {
        bridge_id: filter().bridge,
        runtime: RuntimeIdentity {
            environment: Environment::Production,
            release_id: ReleaseId::new("release").unwrap(),
            release_digest: ArtifactDigest::new("sha256:release").unwrap(),
            process_instance_id: ProcessInstanceId::new("process").unwrap(),
        },
        selected_target_id: None,
    }
}
fn setup(records: usize) -> (CaptureManager, Router, FlowDiagnostics, u64) {
    let manager = CaptureManager::with_ring_limits(true, 8192, records).unwrap();
    let status = manager
        .start(
            &grant("a", vec![CapturePermission::Capture]),
            filter(),
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    let flow = FlowDiagnostics::retained(
        identity().runtime.process_instance_id,
        filter().bridge,
        manager.clone(),
        Arc::new(Clock),
    );
    let router = capture_router(
        identity(),
        ManagementCaptureConfiguration {
            manager: manager.clone(),
            authenticator: Arc::new(Authentication),
        },
    );
    (manager, router, flow, status.id)
}
fn request(token: &str, process: &str, id: u64) -> Request<Body> {
    Request::builder()
        .uri(format!("/api/v1/diagnostics/capture/{process}/{id}/traces"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}
async fn text(response: Response) -> String {
    String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap()
}
fn emit(flow: &FlowDiagnostics) {
    flow.span(None, filter().station, None)
        .emit(FlowStage::Application, FlowEvidence::Completed);
}

#[tokio::test]
async fn route_authenticates_scope_and_separates_restart_expiry_and_durable_cursors() {
    let (_, router, _, id) = setup(8);
    for (token, status) in [
        ("bad", StatusCode::UNAUTHORIZED),
        ("wrong-station", StatusCode::FORBIDDEN),
        ("control", StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            router
                .clone()
                .oneshot(request(token, "process", id))
                .await
                .unwrap()
                .status(),
            status
        );
    }
    for (process, capture, reason) in [
        ("old-process", id, "restart"),
        ("process", id + 1, "expiry"),
    ] {
        let response = router
            .clone()
            .oneshot(request("reader", process, capture))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert!(text(response).await.contains(reason));
    }
    let mut invalid = request("reader", "process", id);
    invalid
        .headers_mut()
        .insert("last-event-id", "durable:event:1".parse().unwrap());
    assert_eq!(
        router.clone().oneshot(invalid).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let mut previous = request("reader", "process", id);
    previous.headers_mut().insert(
        "last-event-id",
        format!("[\"old\",{id},0]").parse().unwrap(),
    );
    assert!(
        text(router.oneshot(previous).await.unwrap())
            .await
            .contains("restart")
    );
}

#[tokio::test]
async fn restart_gaps_do_not_require_an_active_capture() {
    let router = capture_router(
        identity(),
        ManagementCaptureConfiguration {
            manager: CaptureManager::new(true),
            authenticator: Arc::new(Authentication),
        },
    );
    let old_path = request("reader", "old-process", 1);
    let mut old_cursor = request("reader", "process", 1);
    old_cursor
        .headers_mut()
        .insert("last-event-id", "[\"old-process\",1,0]".parse().unwrap());
    for request in [old_path, old_cursor] {
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text(response).await).unwrap(),
            json!({"code":"trace.gap", "reason":"restart", "replay":"best_effort"})
        );
    }
    assert_eq!(
        router
            .oneshot(request("bad", "old-process", 1))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn resumed_cursor_reports_producer_drops_inside_previously_evicted_window() {
    let (manager, router, flow, id) = setup(3);
    for _ in 0..4 {
        emit(&flow);
    }
    let mut reconnect = request("reader", "process", id);
    reconnect.headers_mut().insert(
        "last-event-id",
        format!("[\"process\",{id},2]").parse().unwrap(),
    );
    let response = router.oneshot(reconnect).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    for expected in ["event: trace_window", "event: trace\n"] {
        let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&chunk).unwrap().contains(expected));
    }

    assert!(!manager.try_record(&filter(), |_, _| None));
    emit(&flow);
    let gap = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let gap = std::str::from_utf8(&gap).unwrap();
    assert!(gap.contains("event: trace_gap"));
    assert!(gap.contains("\"reason\":\"dropped\""));
    assert!(!gap.contains("\"reason\":\"overflow\""));
    let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next = std::str::from_utf8(&next).unwrap();
    assert!(next.contains("event: trace\n"));
    assert!(next.contains(&format!("id: [\"process\",{id},5]")));
}

#[tokio::test]
async fn stream_reports_overflow_and_stop_without_replaying_old_capture() {
    let (manager, router, flow, id) = setup(2);
    for _ in 0..8 {
        emit(&flow);
    }
    let response = router
        .clone()
        .oneshot(request("reader", "process", id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let mut stream = response.into_body().into_data_stream();
    let mut observed = String::new();
    for _ in 0..3 {
        let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        observed.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    assert!(observed.contains("event: trace_window"));
    assert!(observed.contains("\"reason\":\"overflow\""));
    assert!(observed.contains("event: trace\n"));
    assert!(!observed.contains("durable"));
    manager
        .stop(&grant("a", vec![CapturePermission::Capture]), id)
        .unwrap();
    let next = manager
        .start(
            &grant("a", vec![CapturePermission::Capture]),
            filter(),
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    emit(&flow);
    let terminal = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&terminal).unwrap().contains("expiry"));
    assert!(stream.next().await.is_none());
    assert_ne!(next.id, id);
}

#[tokio::test]
async fn two_subscribers_share_ring_and_disconnect_releases_slots() {
    let (manager, router, flow, id) = setup(8);
    emit(&flow);
    let first = router
        .clone()
        .oneshot(request("reader", "process", id))
        .await
        .unwrap();
    let second = router
        .clone()
        .oneshot(request("reader", "process", id))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        router
            .clone()
            .oneshot(request("reader", "process", id))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(first);
    drop(second);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let lease = manager
        .lease(&grant("a", vec![CapturePermission::Read]), id, false)
        .unwrap();
    assert_eq!(lease.read_after(None).unwrap().window.retained_records, 1);
    assert_eq!(
        router
            .oneshot(request("reader", "process", id))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[test]
fn cursor_bounds_and_header_query_ambiguity_fail_closed() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", "[\"process\",1,3]".parse().unwrap());
    assert_eq!(cursor(&headers, None, "process", 1), Ok(Some(3)));
    assert!(cursor(&headers, Some("[\"process\",1,3]"), "process", 1).is_err());
    headers.append("last-event-id", "[\"process\",1,4]".parse().unwrap());
    assert!(cursor(&headers, None, "process", 1).is_err());
    assert!(cursor(&HeaderMap::new(), Some(&"x".repeat(513)), "process", 1).is_err());
}
