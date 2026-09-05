use crate::{
    events::EventService,
    reads::{ReadExecutor, SupervisedReads},
    routing::{IntegrationState, integration_router},
    test_support,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, header},
    response::Response,
};
use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tower::ServiceExt;
use uob_application::*;
use uob_contracts::*;

#[derive(Default)]
pub(super) struct Source {
    pub events: Vec<RetainedEventItem<serde_json::Value>>,
    pub stay_open: bool,
    pub fail: Option<TargetPortErrorCode>,
    pub calls: AtomicUsize,
    pub polls: Arc<AtomicUsize>,
    pub drops: Arc<AtomicUsize>,
}
impl TargetQueryPort<serde_json::Value> for Source {
    fn query(&self, _: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<serde_json::Value>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "private source detail",
            ))
        })
    }
    fn subscribe_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<serde_json::Value>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let start = match query.after {
                None => 0,
                Some(cursor) => self
                    .events
                    .iter()
                    .position(|item| item.cursor == cursor)
                    .map(|n| n + 1)
                    .ok_or_else(|| {
                        TargetPortError::new(
                            TargetPortErrorCode::CursorExpired,
                            "private cursor detail",
                        )
                    })?,
            };
            Ok(Box::pin(Subscription {
                events: self.events[start..]
                    .iter()
                    .cloned()
                    .map(Ok)
                    .chain(
                        self.fail
                            .map(|code| Err(TargetPortError::new(code, "private stream detail"))),
                    )
                    .collect(),
                stay_open: self.stay_open,
                polls: self.polls.clone(),
                drops: self.drops.clone(),
                capacity: usize::from(query.limit.get()),
            })
                as TargetRetainedEventStream<serde_json::Value>)
        })
    }
}
struct Subscription {
    events: VecDeque<Result<RetainedEventItem<serde_json::Value>, TargetPortError>>,
    stay_open: bool,
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    capacity: usize,
}
impl TargetSubscription<serde_json::Value> for Subscription {
    fn poll_event(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<serde_json::Value>, TargetPortError>>> {
        let this = self.get_mut();
        this.polls.fetch_add(1, Ordering::SeqCst);
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
impl Drop for Subscription {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

pub(super) fn router(source: Arc<Source>) -> (Router, EventService) {
    let events = EventService::new(source.clone());
    let reads = ReadExecutor::new(
        Arc::new(SupervisedReads::new(source)),
        Duration::from_secs(1),
    );
    (
        integration_router(
            IntegrationState::new(
                test_support::descriptor(),
                test_support::listener_limits(16),
                test_support::scoped_credentials(),
                reads,
            )
            .with_events(events.clone()),
        ),
        events,
    )
}
pub(super) async fn response(
    router: Router,
    query: &str,
    token: &str,
    cursor: Option<&str>,
) -> Response {
    let mut request = Request::builder()
        .uri(format!("/bridge/v1/events?{query}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(cursor) = cursor {
        request = request.header("last-event-id", cursor);
    }
    router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
pub(super) async fn text(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
pub(super) fn item(
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

fn station_resource(station_id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("site-01").unwrap(),
        station_id: StationId::new(station_id).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
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
