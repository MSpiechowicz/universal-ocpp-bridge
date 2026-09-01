use std::{
    collections::VecDeque,
    future::Future,
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    CanonicalQuerySource, Page, PageLimit, RetainedEventQuery, ScopedTargetQueryPort,
    SnapshotCursor, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryPort, TargetQueryResult,
    TargetResourceScope, TargetRetainedEventStream, TargetSubscription,
};
use uob_contracts::{
    AccessMode, AuthenticatedCommandOrigin, BridgeId, CommandLifecycle, CommandResult,
    CommandReturnRoute, Connectivity, ContractVersion, DataPointConstraints, DataPointDescriptor,
    DataPointValue, EventEnvelope, EventId, EventOrigin, EventType, Freshness, PointId,
    PrincipalId, ProcessInstanceId, Quality, QualityLevel, ReleaseId, RequestId,
    ResourceCapabilities, ResourceRef, RuntimeIdentity, SemanticName, StationId, StationSnapshot,
    TargetInstanceId, TypedValue, UtcTimestamp, ValueType,
};

#[derive(Default)]
struct FakeSource {
    query_calls: AtomicUsize,
    subscription_calls: AtomicUsize,
    snapshots: Vec<StationSnapshot>,
    events: Vec<EventEnvelope<String>>,
    descriptor: Option<DataPointDescriptor>,
    value: Option<DataPointValue>,
    command_result: Option<CommandResult>,
    oversized_page: bool,
    oversized_subscription: bool,
}

impl CanonicalQuerySource<String> for FakeSource {
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<String>> {
        self.query_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let result = match query {
                TargetQuery::StationSnapshot(resource) => TargetQueryResult::StationSnapshot(
                    self.snapshots
                        .iter()
                        .find(|snapshot| same_station(&snapshot.station, &resource))
                        .cloned(),
                ),
                TargetQuery::StationSnapshots(query) => {
                    let mut items: Vec<_> = self
                        .snapshots
                        .iter()
                        .filter(|snapshot| authorization.permits_resource(&snapshot.station))
                        .cloned()
                        .collect();
                    if !self.oversized_page {
                        items.truncate(usize::from(query.limit.get()));
                    }
                    TargetQueryResult::StationSnapshots(Page {
                        items,
                        next_cursor: Some(
                            SnapshotCursor::new("snapshot-commit-7").expect("snapshot cursor"),
                        ),
                    })
                }
                TargetQuery::DataPointDescriptor { .. } => {
                    TargetQueryResult::DataPointDescriptor(self.descriptor.clone())
                }
                TargetQuery::DataPointValue { .. } => {
                    TargetQueryResult::DataPointValue(self.value.clone())
                }
                TargetQuery::Capabilities(_) => {
                    TargetQueryResult::Capabilities(Some(ResourceCapabilities::default()))
                }
                TargetQuery::CommandResult(_) => {
                    TargetQueryResult::CommandResult(self.command_result.clone())
                }
                TargetQuery::RetainedEvents(query) => {
                    let items = self
                        .events
                        .iter()
                        .filter(|event| same_station(&event.resource, &query.resource))
                        .take(usize::from(query.limit.get()))
                        .cloned()
                        .collect();
                    TargetQueryResult::RetainedEvents(Page {
                        items,
                        next_cursor: None,
                    })
                }
            };
            Ok(result)
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<String>> {
        self.subscription_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let capacity = if self.oversized_subscription {
                usize::from(query.limit.get()) + 1
            } else {
                usize::from(query.limit.get())
            };
            Ok(Box::pin(FakeSubscription {
                events: self.events.iter().cloned().map(Ok).collect(),
                capacity,
            }) as TargetRetainedEventStream<String>)
        })
    }
}

struct FakeSubscription<E> {
    events: VecDeque<Result<EventEnvelope<E>, TargetPortError>>,
    capacity: usize,
}

impl<E: Send + Unpin> TargetSubscription<E> for FakeSubscription<E> {
    fn poll_event(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<EventEnvelope<E>, TargetPortError>>> {
        Poll::Ready(self.get_mut().events.pop_front())
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn backlog(&self) -> usize {
        self.events.len()
    }
}

struct FakeAdapter {
    queries: Arc<dyn TargetQueryPort<String>>,
}

impl FakeAdapter {
    fn query(&self, query: TargetQuery) -> Result<TargetQueryResult<String>, TargetPortError> {
        block_on(self.queries.query(query))
    }

    fn subscribe(
        &self,
        query: RetainedEventQuery,
    ) -> Result<TargetRetainedEventStream<String>, TargetPortError> {
        block_on(self.queries.subscribe_retained_events(query))
    }
}

#[test]
fn fake_adapter_reads_every_supported_canonical_view_without_storage_access() {
    let station = station_resource("station-a");
    let other_station = station_resource("station-b");
    let point_id = text(PointId::new, "connector.available");
    let descriptor = descriptor(station.clone(), point_id.clone());
    let value = point_value(point_id.clone());
    let command_result = command_result(station.clone());
    let source = Arc::new(FakeSource {
        snapshots: vec![snapshot(station.clone()), snapshot(other_station)],
        events: vec![event(station.clone(), 1)],
        descriptor: Some(descriptor.clone()),
        value: Some(value.clone()),
        command_result: Some(command_result.clone()),
        ..FakeSource::default()
    });
    let port = Arc::new(ScopedTargetQueryPort::new(
        source.clone(),
        authorization(&station, all_permissions()),
    ));
    let adapter = FakeAdapter { queries: port };

    let station_page = adapter
        .query(TargetQuery::StationSnapshots(snapshot_query(10)))
        .expect("scoped station page");
    let TargetQueryResult::StationSnapshots(station_page) = station_page else {
        panic!("expected station page");
    };
    assert_eq!(station_page.items.len(), 1);
    assert!(station_page.next_cursor.is_some());

    assert_eq!(
        adapter
            .query(TargetQuery::StationSnapshot(station.clone()))
            .expect("station snapshot"),
        TargetQueryResult::StationSnapshot(Some(snapshot(station.clone())))
    );
    assert_eq!(
        adapter
            .query(TargetQuery::DataPointDescriptor {
                resource: station.clone(),
                point_id: point_id.clone(),
            })
            .expect("point descriptor"),
        TargetQueryResult::DataPointDescriptor(Some(descriptor))
    );
    assert_eq!(
        adapter
            .query(TargetQuery::DataPointValue {
                resource: station.clone(),
                point_id,
            })
            .expect("point value"),
        TargetQueryResult::DataPointValue(Some(value))
    );
    assert!(matches!(
        adapter.query(TargetQuery::Capabilities(station.clone())),
        Ok(TargetQueryResult::Capabilities(Some(_)))
    ));
    assert_eq!(
        adapter
            .query(TargetQuery::CommandResult(request_id()))
            .expect("command result"),
        TargetQueryResult::CommandResult(Some(command_result))
    );
    assert!(matches!(
        adapter.query(TargetQuery::RetainedEvents(event_query(station.clone(), 10))),
        Ok(TargetQueryResult::RetainedEvents(Page { items, .. })) if items.len() == 1
    ));

    let mut stream = adapter
        .subscribe(event_query(station, 10))
        .expect("retained stream");
    assert_eq!(stream.capacity(), 10);
    assert!(matches!(poll_stream(&mut stream), Poll::Ready(Some(Ok(_)))));
    assert_eq!(source.query_calls.load(Ordering::SeqCst), 7);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cross_station_queries_status_and_subscriptions_are_rejected() {
    let allowed = station_resource("station-a");
    let forbidden = station_resource("station-b");
    let source = Arc::new(FakeSource {
        command_result: Some(command_result(forbidden.clone())),
        events: vec![event(forbidden.clone(), 1)],
        ..FakeSource::default()
    });
    let port =
        ScopedTargetQueryPort::new(source.clone(), authorization(&allowed, all_permissions()));

    let error = block_on(port.query(TargetQuery::StationSnapshot(forbidden.clone())))
        .expect_err("cross-station snapshot must fail");
    assert_eq!(error.code(), TargetPortErrorCode::Unauthorized);
    assert_eq!(source.query_calls.load(Ordering::SeqCst), 0);

    let error = block_on(port.query(TargetQuery::CommandResult(request_id())))
        .expect_err("cross-station command result must fail");
    assert_eq!(error.code(), TargetPortErrorCode::Unauthorized);

    let Err(error) = block_on(port.subscribe_retained_events(event_query(forbidden, 10))) else {
        panic!("cross-station subscription must fail");
    };
    assert_eq!(error.code(), TargetPortErrorCode::Unauthorized);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 0);

    let mut stream = block_on(port.subscribe_retained_events(event_query(allowed, 10)))
        .expect("authorized subscription opens");
    let Poll::Ready(Some(Err(error))) = poll_stream(&mut stream) else {
        panic!("out-of-scope streamed event must fail");
    };
    assert_eq!(error.code(), TargetPortErrorCode::Unauthorized);
    assert_eq!(poll_stream(&mut stream), Poll::Ready(None));
}

#[test]
fn query_permissions_and_bounds_fail_closed() {
    let station = station_resource("station-a");
    let source = Arc::new(FakeSource {
        snapshots: vec![snapshot(station.clone()), snapshot(station.clone())],
        oversized_page: true,
        oversized_subscription: true,
        ..FakeSource::default()
    });
    let snapshots_only = ScopedTargetQueryPort::new(
        source.clone(),
        authorization(&station, vec![TargetQueryPermission::StationSnapshots]),
    );

    let error = block_on(snapshots_only.query(TargetQuery::Capabilities(station.clone())))
        .expect_err("ungranted query class must fail");
    assert_eq!(error.code(), TargetPortErrorCode::Unsupported);
    assert_eq!(source.query_calls.load(Ordering::SeqCst), 0);
    assert!(PageLimit::new(101).is_err());

    let error = block_on(snapshots_only.query(TargetQuery::StationSnapshots(snapshot_query(1))))
        .expect_err("source cannot exceed the requested page size");
    assert_eq!(error.code(), TargetPortErrorCode::InvalidRequest);

    let event_port = ScopedTargetQueryPort::new(
        source,
        authorization(&station, vec![TargetQueryPermission::RetainedEvents]),
    );
    let Err(error) = block_on(event_port.subscribe_retained_events(event_query(station, 1))) else {
        panic!("oversized source buffer must fail");
    };
    assert_eq!(error.code(), TargetPortErrorCode::InvalidRequest);
}

fn authorization(
    station: &ResourceRef,
    permissions: Vec<TargetQueryPermission>,
) -> TargetQueryAuthorization {
    TargetQueryAuthorization::new(
        text(TargetInstanceId::new, "target-main"),
        permissions,
        vec![TargetResourceScope::Station {
            bridge_id: station.bridge_id.clone(),
            station_id: station.station_id.clone(),
        }],
    )
}

fn all_permissions() -> Vec<TargetQueryPermission> {
    vec![
        TargetQueryPermission::StationSnapshots,
        TargetQueryPermission::DataPoints,
        TargetQueryPermission::Capabilities,
        TargetQueryPermission::CommandStatus,
        TargetQueryPermission::RetainedEvents,
    ]
}

fn snapshot_query(limit: u16) -> uob_application::SnapshotQuery {
    uob_application::SnapshotQuery {
        after: None,
        limit: PageLimit::new(limit).expect("valid page limit"),
    }
}

fn event_query(resource: ResourceRef, limit: u16) -> RetainedEventQuery {
    RetainedEventQuery {
        resource,
        after: None,
        limit: PageLimit::new(limit).expect("valid page limit"),
    }
}

fn station_resource(station: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-1"),
        station_id: text(StationId::new, station),
        resource: None,
        native_protocol_reference: None,
    }
}

fn same_station(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.bridge_id == right.bridge_id && left.station_id == right.station_id
}

fn snapshot(station: ResourceRef) -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station,
        observed_at: timestamp(0),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: Vec::new(),
        transactions: Vec::new(),
        current_values: Vec::new(),
    }
}

fn descriptor(resource: ResourceRef, point_id: PointId) -> DataPointDescriptor {
    DataPointDescriptor {
        point_id,
        resource,
        semantic_name: text(SemanticName::new, "connector.available.v1"),
        value_type: ValueType::Boolean,
        unit: None,
        access: AccessMode::ReadOnly,
        constraints: DataPointConstraints::default(),
    }
}

fn point_value(point_id: PointId) -> DataPointValue {
    DataPointValue {
        point_id,
        value: Some(TypedValue::Boolean(true)),
        source_time: None,
        observed_at: timestamp(0),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    }
}

fn command_result(resource: ResourceRef) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: None,
        resource,
        return_route: CommandReturnRoute {
            request_id: request_id(),
            origin: AuthenticatedCommandOrigin::Target {
                target_instance_id: text(TargetInstanceId::new, "target-main"),
                principal_id: text(PrincipalId::new, "reader-1"),
            },
        },
        lifecycle: CommandLifecycle::Admitted,
        recorded_at: timestamp(0),
        observed_effects: Vec::new(),
    }
}

fn request_id() -> RequestId {
    text(RequestId::new, "request-1")
}

fn event(resource: ResourceRef, sequence: u64) -> EventEnvelope<String> {
    EventEnvelope {
        event_id: text(EventId::new, format!("event-{sequence}")),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: uob_contracts::Environment::Demo,
            release_id: text(ReleaseId::new, "release-1"),
            release_digest: text(uob_contracts::ArtifactDigest::new, "sha256:abc"),
            process_instance_id: text(ProcessInstanceId::new, "process-1"),
        },
        resource,
        source_time: None,
        observed_at: timestamp(0),
        event_type: text(EventType::new, "station.changed.v1"),
        origin: EventOrigin::Station,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: "changed".to_owned(),
    }
}

fn timestamp(minute: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).expect("fixture date"),
            Time::from_hms(12, minute, 0).expect("fixture time"),
        )
        .assume_offset(UtcOffset::UTC),
    )
}

fn text<T, E: std::fmt::Debug>(
    constructor: impl FnOnce(String) -> Result<T, E>,
    value: impl Into<String>,
) -> T {
    constructor(value.into()).expect("valid identity")
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn poll_stream<E>(
    stream: &mut TargetRetainedEventStream<E>,
) -> Poll<Option<Result<EventEnvelope<E>, TargetPortError>>> {
    let mut context = Context::from_waker(Waker::noop());
    stream.as_mut().poll_event(&mut context)
}
