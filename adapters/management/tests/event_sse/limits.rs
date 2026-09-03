use super::*;

#[tokio::test]
async fn stalled_subscriber_is_bounded_and_does_not_delay_snapshot_reads() {
    let source = Arc::new(EventSource {
        events: (1..=20)
            .map(|value| {
                item(
                    &value.to_string(),
                    &format!("event-{value}"),
                    "station-a",
                    "station.changed.v1",
                )
            })
            .collect(),
        ..EventSource::default()
    });
    let limits = ManagementEventLimits {
        subscriber_capacity: 2,
        slow_subscriber_timeout: Duration::from_millis(20),
        ..ManagementEventLimits::default()
    };
    let (router, application) = test_router(source.clone(), limits);
    let stalled = router
        .clone()
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(source.polled_events.load(Ordering::SeqCst), 3);
    let runtime = application.health().snapshot().runtime;
    assert_eq!(runtime.queues.subscribers, 1);
    assert!(runtime.queued_payload_bytes > 0);
    let charger = application
        .health()
        .resources()
        .try_reserve(WorkClass::ChargerRequest, 1)
        .expect("subscriber pressure must preserve charger admission");
    let target = application
        .health()
        .resources()
        .try_reserve(WorkClass::TargetEgress, 1)
        .expect("subscriber pressure must preserve target delivery admission");
    drop((charger, target));

    let snapshot = tokio::time::timeout(
        Duration::from_millis(50),
        router.oneshot(event_request("/api/v1/stations/station-a", None)),
    )
    .await
    .expect("snapshot response must not wait for the stalled subscriber")
    .unwrap();
    assert_eq!(snapshot.status(), StatusCode::OK);
    drop(stalled);
    tokio::task::yield_now().await;
    let runtime = application.health().snapshot().runtime;
    assert_eq!(runtime.queues.subscribers, 0);
    assert_eq!(runtime.queued_payload_bytes, 0);
}

#[tokio::test]
async fn oversized_serialized_event_is_rejected_inside_the_stream_bound() {
    let mut oversized = item("1", "large-event", "station-a", "station.changed.v1");
    oversized.event.payload = serde_json::json!({ "content": "x".repeat(4 * 1024) });
    let source = Arc::new(EventSource {
        events: vec![oversized],
        ..EventSource::default()
    });
    let limits = ManagementEventLimits {
        maximum_payload_bytes: 128,
        ..ManagementEventLimits::default()
    };
    let (router, application) = test_router(source, limits);

    let response = router
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains("event: error"));
    assert!(body.contains("events.payload_too_large"));
    assert!(!body.contains(&"x".repeat(256)));
    assert_eq!(
        application.health().snapshot().runtime.queues.subscribers,
        0
    );
}

#[tokio::test]
async fn filter_count_is_rejected_before_opening_the_source() {
    let source = Arc::new(EventSource::default());
    let (router, _) = test_router(source.clone(), ManagementEventLimits::default());
    let filters = (0..9)
        .map(|value| format!("event.{value}.v1"))
        .collect::<Vec<_>>()
        .join(",");

    let response = router
        .oneshot(event_request(
            &format!("/api/v1/events?types={filters}"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_subscriber_limit_is_held_for_the_connection_lifetime() {
    let source = Arc::new(EventSource {
        stay_open: true,
        ..EventSource::default()
    });
    let limits = ManagementEventLimits {
        maximum_concurrent_subscribers: 1,
        ..ManagementEventLimits::default()
    };
    let (router, application) = test_router(source.clone(), limits);
    let first = router
        .clone()
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        application.health().snapshot().runtime.queues.subscribers,
        1
    );

    let rejected = router
        .clone()
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 1);

    drop(first);
    tokio::time::timeout(Duration::from_millis(100), async {
        while application.health().snapshot().runtime.queues.subscribers != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping the client must release subscriber admission");
    let replacement = router
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(replacement.status(), StatusCode::OK);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 2);
    drop(replacement);
}
