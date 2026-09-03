use std::{
    collections::{BTreeSet, VecDeque},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use futures_util::StreamExt;
use tower::ServiceExt;
use uob_application::{
    Application, CanonicalQuerySource, CoreLoopState, Page, RetainedEventCursor, RetainedEventItem,
    RetainedEventQuery, SnapshotCursor, StorageHealthState, TargetPortError, TargetPortErrorCode,
    TargetPortFuture, TargetQuery, TargetQueryAuthorization, TargetQueryPermission,
    TargetQueryResult, TargetResourceScope, TargetRetainedEventStream, TargetSubscription,
    WorkClass,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource,
    Connectivity, ContractVersion, Environment, EventEnvelope, EventId, EventOrigin, EventType,
    NativeProtocolReference, ProcessInstanceId, ReleaseId, ResourceCapabilities, ResourceRef,
    RuntimeIdentity, ServiceIdentity, StationId, StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_management_adapter::{
    AuthenticatedEventAccess, ManagementEventAuthenticator, ManagementEventConfiguration,
    ManagementEventLimits, ManagementReadLimits, ManagementRouterOptions,
    router_with_authenticated_events,
};

#[path = "event_sse/access.rs"]
mod access;
#[path = "event_sse/combined.rs"]
mod combined;
#[path = "event_sse/limits.rs"]
mod limits;

#[derive(Default)]
struct EventSource {
    events: Vec<RetainedEventItem<serde_json::Value>>,
    expired: BTreeSet<String>,
    stream_error: Option<TargetPortErrorCode>,
    stay_open: bool,
    subscription_calls: AtomicUsize,
    query_calls: AtomicUsize,
    polled_events: Arc<AtomicUsize>,
    cursors: Mutex<Vec<Option<String>>>,
}

impl CanonicalQuerySource<serde_json::Value> for EventSource {
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<serde_json::Value>> {
        self.query_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match query {
                TargetQuery::StationSnapshot(resource) => Ok(TargetQueryResult::StationSnapshot(
                    authorization
                        .permits_resource(&resource)
                        .then(|| snapshot(resource)),
                )),
                TargetQuery::StationSnapshots(query) => {
                    Ok(TargetQueryResult::StationSnapshots(Page {
                        items: vec![snapshot(station_resource("station-a"))]
                            .into_iter()
                            .filter(|item| authorization.permits_resource(&item.station))
                            .take(usize::from(query.limit.get()))
                            .collect(),
                        next_cursor: None::<SnapshotCursor>,
                    }))
                }
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "query.unsupported",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<serde_json::Value>> {
        self.subscription_calls.fetch_add(1, Ordering::SeqCst);
        let after = query.after.as_ref().map(|value| value.as_str().to_owned());
        self.cursors.lock().unwrap().push(after.clone());
        let result =
            if after
                .as_ref()
                .is_some_and(|cursor| self.expired.contains(cursor))
            {
                Err(TargetPortError::new(
                    TargetPortErrorCode::CursorExpired,
                    "storage detail must not cross the adapter",
                ))
            } else {
                let start = match after {
                    None => Some(0),
                    Some(cursor) => self
                        .events
                        .iter()
                        .position(|item| item.cursor.as_str() == cursor)
                        .map(|index| index + 1),
                };
                match start {
                    Some(start) => Ok(Box::pin(FakeSubscription {
                        events: self.events[start..]
                            .iter()
                            .cloned()
                            .map(Ok)
                            .chain(self.stream_error.map(|code| {
                                Err(TargetPortError::new(code, "stream source detail"))
                            }))
                            .collect(),
                        capacity: usize::from(query.limit.get()),
                        polled_events: Arc::clone(&self.polled_events),
                        stay_open: self.stay_open,
                    })
                        as TargetRetainedEventStream<serde_json::Value>),
                    None => Err(TargetPortError::new(
                        TargetPortErrorCode::CursorExpired,
                        "unknown cursor",
                    )),
                }
            };
        Box::pin(async move { result })
    }
}

struct FakeSubscription<E> {
    events: VecDeque<Result<RetainedEventItem<E>, TargetPortError>>,
    capacity: usize,
    polled_events: Arc<AtomicUsize>,
    stay_open: bool,
}

impl<E: Send + Unpin> TargetSubscription<E> for FakeSubscription<E> {
    fn poll_event(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<E>, TargetPortError>>> {
        let this = self.get_mut();
        this.polled_events.fetch_add(1, Ordering::SeqCst);
        match this.events.pop_front() {
            Some(event) => Poll::Ready(Some(event)),
            None if this.stay_open => Poll::Pending,
            None => Poll::Ready(None),
        }
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn backlog(&self) -> usize {
        self.events.len()
    }
}

struct TokenAuthenticator {
    access: AuthenticatedEventAccess,
}

impl ManagementEventAuthenticator for TokenAuthenticator {
    fn authenticate(&self, bearer_token: &str) -> Option<AuthenticatedEventAccess> {
        (bearer_token == "reader-secret").then(|| self.access.clone())
    }
}

#[tokio::test]
async fn reconnect_uses_exact_durable_cursor_without_duplicates() {
    let source = Arc::new(EventSource {
        events: vec![
            item("101", "business-one", "station-a", "station.changed.v1"),
            item("150", "filtered-event", "station-a", "meter.changed.v1"),
            item("205", "business-two", "station-a", "station.changed.v1"),
        ],
        ..EventSource::default()
    });
    let (router, _) = test_router(source.clone(), ManagementEventLimits::default());
    let response = router
        .clone()
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut body = response.into_body().into_data_stream();
    let first = body.next().await.unwrap().unwrap();
    let first = std::str::from_utf8(&first).unwrap();
    assert!(first.contains("id: uob:event:101"));
    assert!(first.contains("business-one"));
    assert!(!first.contains("id: business-one"));
    drop(body);

    let response = router
        .oneshot(event_request(
            "/api/v1/events?types=station.changed.v1",
            Some(("last-event-id", "uob:event:101")),
        ))
        .await
        .unwrap();
    let resumed = body_text(response).await;
    assert!(!resumed.contains("business-one"));
    assert!(!resumed.contains("filtered-event"));
    assert!(resumed.contains("id: uob:event:150"));
    assert!(resumed.contains("business-two"));
    assert!(resumed.contains("id: uob:event:205"));
    assert_eq!(
        source.cursors.lock().unwrap().as_slice(),
        &[None, Some("uob:event:101".to_owned())]
    );
}

#[tokio::test]
async fn expired_cursor_returns_gap_and_snapshot_recovery_then_allows_fresh_stream() {
    let resource = evse_resource("station-a", "evse 1", Some("connector/1"));
    let source = Arc::new(EventSource {
        events: vec![resource_item(
            "12",
            "fresh-event",
            resource.clone(),
            "station.changed.v1",
        )],
        expired: BTreeSet::from(["uob:event:gone".to_owned()]),
        ..EventSource::default()
    });
    let (router, _) = test_router(source.clone(), ManagementEventLimits::default());
    let expired = router
        .clone()
        .oneshot(event_request(
            "/api/v1/events?station_id=station-a&evse_id=evse%201&connector_id=connector%2F1&types=station.changed.v1&after=uob%3Aevent%3Agone",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::GONE);
    let gap: serde_json::Value = serde_json::from_str(&body_text(expired).await).unwrap();
    assert_eq!(gap["kind"], "durable_cursor_gap");
    assert_eq!(gap["error"], "events.cursor_expired");
    assert_eq!(gap["recovery"]["action"], "fetch_fresh_snapshot");
    assert_eq!(
        gap["recovery"]["snapshot_url"],
        "/api/v1/stations/station-a"
    );
    assert_eq!(
        gap["recovery"]["resubscribe_url"],
        "/api/v1/events?station_id=station-a&evse_id=evse%201&connector_id=connector%2F1&types=station.changed.v1"
    );
    assert_eq!(gap["recovery"]["resource"], serde_json::json!(resource));
    assert_eq!(
        gap["recovery"]["event_types"],
        serde_json::json!(["station.changed.v1"])
    );
    assert_eq!(gap["recovery"]["omit_cursor"], true);

    let snapshot = router
        .clone()
        .oneshot(event_request(
            gap["recovery"]["snapshot_url"].as_str().unwrap(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(snapshot.status(), StatusCode::OK);
    let fresh = router
        .oneshot(event_request(
            gap["recovery"]["resubscribe_url"].as_str().unwrap(),
            None,
        ))
        .await
        .unwrap();
    assert!(body_text(fresh).await.contains("fresh-event"));
}

#[tokio::test]
async fn cursor_expiry_after_headers_is_a_named_sse_gap_without_an_id() {
    let source = Arc::new(EventSource {
        stream_error: Some(TargetPortErrorCode::CursorExpired),
        ..EventSource::default()
    });
    let (router, _) = test_router(source, ManagementEventLimits::default());
    let response = router
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains("event: gap"));
    assert!(body.contains("durable_cursor_gap"));
    assert!(body.contains("fetch_fresh_snapshot"));
    assert!(!body.contains("id:"));
}

fn test_router(
    source: Arc<EventSource>,
    limits: ManagementEventLimits,
) -> (axum::Router, Application) {
    let resource = station_resource("station-a");
    let authorization = authorization(&resource);
    test_router_with_access(
        source,
        limits,
        AuthenticatedEventAccess {
            authorization,
            default_resource: resource,
        },
    )
}

fn test_router_with_access(
    source: Arc<EventSource>,
    limits: ManagementEventLimits,
    access: AuthenticatedEventAccess,
) -> (axum::Router, Application) {
    let application = application();
    let authenticator = Arc::new(TokenAuthenticator { access });
    let router = router_with_authenticated_events(
        application.clone(),
        source,
        ManagementReadLimits::default(),
        ManagementEventConfiguration {
            authenticator,
            limits,
        },
        ManagementRouterOptions::default(),
    );
    (router, application)
}

fn event_request(uri: &str, extra_header: Option<(&str, &str)>) -> Request<Body> {
    let mut request = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer reader-secret");
    if let Some((name, value)) = extra_header {
        request = request.header(name, value);
    }
    request.body(Body::empty()).unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 512 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn authorization(resource: &ResourceRef) -> TargetQueryAuthorization {
    TargetQueryAuthorization::new(
        TargetInstanceId::new("management-events").unwrap(),
        vec![
            TargetQueryPermission::RetainedEvents,
            TargetQueryPermission::StationSnapshots,
        ],
        vec![TargetResourceScope::Station {
            bridge_id: resource.bridge_id.clone(),
            station_id: resource.station_id.clone(),
        }],
    )
}

fn item(
    cursor: &str,
    event_id: &str,
    station_id: &str,
    event_type: &str,
) -> RetainedEventItem<serde_json::Value> {
    resource_item(cursor, event_id, station_resource(station_id), event_type)
}

fn resource_item(
    cursor: &str,
    event_id: &str,
    resource: ResourceRef,
    event_type: &str,
) -> RetainedEventItem<serde_json::Value> {
    RetainedEventItem {
        cursor: RetainedEventCursor::new(format!("uob:event:{cursor}")).unwrap(),
        event: EventEnvelope {
            event_id: EventId::new(event_id).unwrap(),
            schema_version: ContractVersion::V1_INITIAL,
            runtime: runtime(),
            resource,
            source_time: None,
            observed_at: timestamp(),
            event_type: EventType::new(event_type).unwrap(),
            origin: EventOrigin::Station,
            sequence: cursor.parse().unwrap_or(1),
            correlation_id: None,
            causation_id: None,
            provenance: None,
            payload: serde_json::json!({ "event": event_id }),
        },
    }
}

fn snapshot(station: ResourceRef) -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station,
        observed_at: timestamp(),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        transactions: vec![],
        current_values: vec![],
    }
}

fn station_resource(station_id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        station_id: StationId::new(station_id).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

fn evse_resource(station_id: &str, evse_id: &str, connector_id: Option<&str>) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        station_id: StationId::new(station_id).unwrap(),
        resource: Some(CanonicalResource::Evse {
            evse_id: CanonicalEvseId::new(evse_id).unwrap(),
            connector_id: connector_id
                .map(CanonicalConnectorId::new)
                .transpose()
                .unwrap(),
        }),
        native_protocol_reference: None,
    }
}

fn application() -> Application {
    let application = Application::new(ServiceIdentity {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        runtime: runtime(),
        selected_target_id: None,
    });
    application.health().report_core_loop(CoreLoopState::Ready);
    application
        .health()
        .report_storage(StorageHealthState::Safe, None);
    application
}

fn runtime() -> RuntimeIdentity {
    RuntimeIdentity {
        environment: Environment::Production,
        release_id: ReleaseId::new("release-api").unwrap(),
        release_digest: ArtifactDigest::new("sha256:api").unwrap(),
        process_instance_id: ProcessInstanceId::new("process-api").unwrap(),
    }
}

fn timestamp() -> UtcTimestamp {
    serde_json::from_str("\"2026-09-03T00:00:00Z\"").unwrap()
}
