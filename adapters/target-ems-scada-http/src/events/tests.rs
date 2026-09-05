mod support;
use super::{DEADLINE, QUEUE_CAPACITY};
use axum::http::StatusCode;
use futures_util::StreamExt;
use std::sync::{Arc, atomic::Ordering};
use support::{Source, item, response, router, text};
use uob_application::TargetPortErrorCode;

#[tokio::test]
async fn filtered_reconnect_keeps_per_record_checkpoints_and_no_duplicates() {
    let source = Arc::new(Source {
        events: vec![
            item("101", "one", "station-a", "status.v1"),
            item("150", "filtered", "station-a", "meter.v1"),
            item("205", "two", "station-a", "status.v1"),
        ],
        ..Source::default()
    });
    let (router, _) = router(source);
    let first = response(
        router.clone(),
        "station_id=station-a&types=status.v1",
        "reader-token",
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["content-type"], "text/event-stream");
    let body = text(first).await;
    assert!(body.contains("id: uob:event:101"));
    assert!(body.contains("id: uob:event:150"));
    assert!(!body.contains("filtered"));
    assert_eq!(body.matches("event: durable").count(), 2);
    let resumed = text(
        response(
            router,
            "station_id=station-a&types=status.v1",
            "reader-token",
            Some("uob:event:150"),
        )
        .await,
    )
    .await;
    assert!(!resumed.contains("uob:event:101"));
    assert!(!resumed.contains("uob:event:150"));
    assert!(resumed.contains("uob:event:205"));
}

#[tokio::test]
async fn expired_cursor_and_midstream_gap_require_explicit_snapshot_recovery() {
    let (router, _) = router(Arc::new(Source {
        fail: Some(TargetPortErrorCode::CursorExpired),
        ..Source::default()
    }));
    let expired = response(
        router.clone(),
        "station_id=station-a&types=status.v1",
        "reader-token",
        Some("uob:event:expired"),
    )
    .await;
    assert_eq!(expired.status(), StatusCode::GONE);
    let body = text(expired).await;
    assert!(body.contains("fetch_fresh_snapshot"));
    assert!(body.contains("/bridge/v1/stations/"));
    assert!(body.contains("status.v1"));
    assert!(!body.contains("private"));
    let gap = text(response(router, "station_id=station-a", "reader-token", None).await).await;
    assert!(gap.contains("event: gap"));
    assert!(gap.contains("omit_cursor"));
    assert!(!gap.contains("id:"));
}

#[tokio::test]
async fn unauthorized_and_malformed_subscriptions_never_open_the_source() {
    let source = Arc::new(Source::default());
    let (router, _) = router(source.clone());
    for (query, token, cursor, status) in [
        (
            "station_id=station-a",
            "bad",
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "station_id=station-a",
            "controller-token",
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            "station_id=station-b",
            "station-scoped-token",
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            "station_id=station-a&after=uob:event:1",
            "reader-token",
            Some("uob:event:2"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "station_id=station-a",
            "reader-token",
            Some("trace:1"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "station_id=station-a&types=",
            "reader-token",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            "station_id=station-a&secret=echo",
            "reader-token",
            None,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            response(router.clone(), query, token, cursor)
                .await
                .status(),
            status
        );
    }
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn source_scope_violation_and_oversized_payload_close_without_leaking_data() {
    let mut large = item("1", "large", "station-a", "status.v1");
    large.event.payload = serde_json::json!({"secret": "X".repeat(super::MAX_PAYLOAD_BYTES)});
    for event in [item("2", "forbidden", "station-b", "status.v1"), large] {
        let (router, _) = router(Arc::new(Source {
            events: vec![event],
            ..Source::default()
        }));
        let body =
            text(response(router, "station_id=station-a", "station-scoped-token", None).await)
                .await;
        assert!(body.contains("event: error"));
        assert!(!body.contains("forbidden"));
        assert!(!body.contains("secret"));
        assert!(!body.contains("id:"));
    }
}

#[tokio::test]
async fn stalled_readers_are_bounded_and_release_sources_without_blocking_other_queries() {
    let source = Arc::new(Source {
        events: (1..100)
            .map(|n| item(&n.to_string(), "event", "station-a", "status.v1"))
            .collect(),
        ..Source::default()
    });
    let (router, _) = router(source.clone());
    let first = response(router.clone(), "station_id=station-a", "reader-token", None).await;
    let second = response(router.clone(), "station_id=station-a", "reader-token", None).await;
    assert_eq!(
        response(router.clone(), "station_id=station-a", "reader-token", None)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    tokio::time::sleep(DEADLINE + std::time::Duration::from_millis(100)).await;
    assert_eq!(source.drops.load(Ordering::SeqCst), 2);
    assert_eq!(
        source.polls.load(Ordering::SeqCst),
        2 * (usize::from(QUEUE_CAPACITY) + 1)
    );
    let (status, capabilities) = crate::test_support::get(
        router.clone(),
        "/bridge/v1/capabilities",
        Some("reader-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(capabilities["events"]["delivery"], "local_exposure");
    assert_eq!(
        capabilities["events"]["telemetry"],
        "best_effort_not_in_durable_stream"
    );
    drop(first);
    drop(second);
    assert_eq!(
        response(router, "station_id=station-a", "reader-token", None)
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn disconnect_and_session_shutdown_cancel_idle_sources_and_stream_bodies() {
    let source = Arc::new(Source {
        stay_open: true,
        ..Source::default()
    });
    let (router, service) = router(source.clone());
    let first = response(router.clone(), "station_id=station-a", "reader-token", None).await;
    tokio::task::yield_now().await;
    drop(first);
    tokio::task::yield_now().await;
    assert_eq!(source.drops.load(Ordering::SeqCst), 1);
    let second = response(router.clone(), "station_id=station-a", "reader-token", None).await;
    service.shutdown();
    let mut body = second.into_body().into_data_stream();
    assert!(body.next().await.is_none());
    tokio::task::yield_now().await;
    assert_eq!(source.drops.load(Ordering::SeqCst), 2);
    assert_eq!(
        response(router, "station_id=station-a", "reader-token", None)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn canonical_resource_filters_and_recovery_urls_preserve_scope() {
    for resource in [
        uob_contracts::CanonicalResource::Connector {
            connector_id: uob_contracts::CanonicalConnectorId::new("cable/1").unwrap(),
        },
        uob_contracts::CanonicalResource::Evse {
            evse_id: uob_contracts::CanonicalEvseId::new("evse & one").unwrap(),
            connector_id: None,
        },
    ] {
        let mut event = item("42", "resource-event", "station-a", "status.v1");
        event.event.resource.resource = Some(resource.clone());
        let filter = match resource {
            uob_contracts::CanonicalResource::Connector { .. } => "connector_id=cable%2F1",
            uob_contracts::CanonicalResource::Evse { .. } => "evse_id=evse%20%26%20one",
        };
        let query = format!("station_id=station-a&{filter}&types=status.v1");
        let (router, _) = router(Arc::new(Source {
            events: vec![event],
            ..Source::default()
        }));
        let body = text(response(router.clone(), &query, "reader-token", None).await).await;
        assert!(body.contains("resource-event"));
        let expired = response(router, &query, "reader-token", Some("uob:event:expired")).await;
        let recovery: serde_json::Value = serde_json::from_str(&text(expired).await).unwrap();
        assert_eq!(
            recovery["recovery"]["snapshot_url"],
            "/bridge/v1/stations/station-a?bridge_id=site-01"
        );
        assert_eq!(
            recovery["recovery"]["resubscribe_url"],
            format!("/bridge/v1/events?bridge_id=site-01&{query}")
        );
    }
}
