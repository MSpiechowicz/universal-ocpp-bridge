use super::*;

#[tokio::test]
async fn authentication_and_resource_scope_fail_closed_before_or_during_emission() {
    let source = Arc::new(EventSource {
        events: vec![item(
            "1",
            "forbidden-payload",
            "station-b",
            "station.changed.v1",
        )],
        ..EventSource::default()
    });
    let (router, _) = test_router(source.clone(), ManagementEventLimits::default());
    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let forbidden = router
        .clone()
        .oneshot(event_request("/api/v1/events?station_id=station-b", None))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 0);

    let malicious = router
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    let body = body_text(malicious).await;
    assert!(body.contains("events.resource_unauthorized"));
    assert!(!body.contains("station-b"));
    assert!(!body.contains("forbidden-payload"));
}

#[tokio::test]
async fn event_only_grant_is_rejected_before_source_touch() {
    let source = Arc::new(EventSource::default());
    let resource = station_resource("station-a");
    let access = AuthenticatedEventAccess {
        authorization: TargetQueryAuthorization::new(
            TargetInstanceId::new("events-only").unwrap(),
            vec![TargetQueryPermission::RetainedEvents],
            vec![TargetResourceScope::Station {
                bridge_id: resource.bridge_id.clone(),
                station_id: resource.station_id.clone(),
            }],
        ),
        default_resource: resource,
    };
    let (router, _) =
        test_router_with_access(source.clone(), ManagementEventLimits::default(), access);

    let response = router
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(response)
            .await
            .contains("snapshot_recovery_not_granted")
    );
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 0);
    assert_eq!(source.query_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_child_grant_without_station_snapshot_scope_is_rejected() {
    let source = Arc::new(EventSource::default());
    let child = evse_resource("station-a", "evse-a", Some("connector-a"));
    let access = AuthenticatedEventAccess {
        authorization: TargetQueryAuthorization::new(
            TargetInstanceId::new("exact-child").unwrap(),
            vec![
                TargetQueryPermission::RetainedEvents,
                TargetQueryPermission::StationSnapshots,
            ],
            vec![TargetResourceScope::Resource(child.clone())],
        ),
        default_resource: child,
    };
    let (router, _) =
        test_router_with_access(source.clone(), ManagementEventLimits::default(), access);

    let response = router
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(response)
            .await
            .contains("snapshot_recovery_not_granted")
    );
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_authorized_resource_does_not_depend_on_default_resource() {
    let source = Arc::new(EventSource::default());
    let resource = station_resource("station-a");
    let access = AuthenticatedEventAccess {
        authorization: authorization(&resource),
        default_resource: ResourceRef {
            bridge_id: BridgeId::new("another-bridge").unwrap(),
            station_id: StationId::new("irrelevant-default").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
    };
    let (router, _) =
        test_router_with_access(source.clone(), ManagementEventLimits::default(), access);

    let response = router
        .oneshot(event_request("/api/v1/events?station_id=station-a", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn default_resource_native_address_is_not_exposed_in_recovery() {
    let source = Arc::new(EventSource {
        expired: BTreeSet::from(["uob:event:gone".to_owned()]),
        ..EventSource::default()
    });
    let mut default_resource = station_resource("station-a");
    default_resource.native_protocol_reference =
        Some(NativeProtocolReference::Ocpp16 { connector_id: 7 });
    let access = AuthenticatedEventAccess {
        authorization: authorization(&default_resource),
        default_resource,
    };
    let (router, _) = test_router_with_access(source, ManagementEventLimits::default(), access);

    let response = router
        .oneshot(event_request(
            "/api/v1/events?after=uob%3Aevent%3Agone",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GONE);
    let gap: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert!(
        gap["recovery"]["resource"]
            .get("native_protocol_reference")
            .is_none()
    );
}

#[tokio::test]
async fn exact_child_filter_does_not_leak_an_authorized_sibling() {
    let sibling = evse_resource("station-a", "evse-a", Some("connector-b"));
    let source = Arc::new(EventSource {
        events: vec![resource_item(
            "1",
            "sibling-payload",
            sibling,
            "station.changed.v1",
        )],
        ..EventSource::default()
    });
    let (router, _) = test_router(source, ManagementEventLimits::default());

    let response = router
        .oneshot(event_request(
            "/api/v1/events?station_id=station-a&evse_id=evse-a&connector_id=connector-a",
            None,
        ))
        .await
        .unwrap();
    let body = body_text(response).await;
    assert!(body.contains("events.resource_unauthorized"));
    assert!(!body.contains("sibling-payload"));
    assert!(!body.contains("connector-b"));
}
