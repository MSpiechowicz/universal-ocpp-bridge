use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    CanonicalQuerySource, CommandHistoryQuery, Page, PageLimit, RetainedEventQuery,
    ScopedTargetQueryPort, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryPort, TargetQueryResult,
    TargetResourceScope, TargetRetainedEventStream,
};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalResource, CommandOperationKind, CommandSummary,
    RequestId, ResourceRef, StationId, TargetInstanceId, UtcTimestamp,
};

struct Source {
    calls: AtomicUsize,
    result: CommandSummary,
}
impl CanonicalQuerySource<()> for Source {
    fn query<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        _query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<()>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(TargetQueryResult::CommandHistory(Page {
                items: vec![self.result.clone()],
                next_cursor: None,
            }))
        })
    }
    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        _query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<()>> {
        Box::pin(async { unreachable!("history queries never subscribe") })
    }
}

#[test]
fn unauthorized_read_is_denied_before_source_and_foreign_child_is_rejected_after_source() {
    let station = station_resource("station-a");
    let granted = child(station.clone(), 1);
    let foreign = child(station.clone(), 2);
    let source = Arc::new(Source {
        calls: AtomicUsize::new(0),
        result: summary(foreign),
    });
    let authorization = |permissions| {
        TargetQueryAuthorization::new(
            TargetInstanceId::new("reader").unwrap(),
            permissions,
            vec![TargetResourceScope::Resource(granted.clone())],
        )
    };
    let query = || {
        TargetQuery::CommandHistory(CommandHistoryQuery {
            station: station.clone(),
            after: None,
            limit: PageLimit::new(1).unwrap(),
        })
    };
    let no_permission = ScopedTargetQueryPort::new(source.clone(), authorization(vec![]));
    let denied = block_on(no_permission.query(query())).unwrap_err();
    assert_eq!(denied.code(), TargetPortErrorCode::Unsupported);
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    let permitted = ScopedTargetQueryPort::new(
        source.clone(),
        authorization(vec![TargetQueryPermission::CommandStatus]),
    );
    let foreign = block_on(permitted.query(query())).unwrap_err();
    assert_eq!(foreign.code(), TargetPortErrorCode::Unauthorized);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    let other_station = TargetQuery::CommandHistory(CommandHistoryQuery {
        station: station_resource("station-b"),
        after: None,
        limit: PageLimit::new(1).unwrap(),
    });
    assert_eq!(
        block_on(permitted.query(other_station)).unwrap_err().code(),
        TargetPortErrorCode::Unauthorized
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

fn station_resource(id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-a").unwrap(),
        station_id: StationId::new(id).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
fn child(mut station: ResourceRef, id: u32) -> ResourceRef {
    station.resource = Some(CanonicalResource::Connector {
        connector_id: CanonicalConnectorId::new(id.to_string()).unwrap(),
    });
    station
}
fn summary(resource: ResourceRef) -> CommandSummary {
    let now = UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).unwrap(),
            Time::MIDNIGHT,
        )
        .assume_offset(UtcOffset::UTC),
    );
    CommandSummary {
        request_id: RequestId::new("known").unwrap(),
        correlation_id: None,
        resource,
        operation: CommandOperationKind::Start,
        admitted_at: now,
        expires_at: now,
        lifecycle: None,
        recorded_at: None,
        observed_effects: vec![],
    }
}
fn block_on<F: Future>(future: F) -> F::Output {
    struct WakeThread(std::thread::Thread);
    impl Wake for WakeThread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(WakeThread(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}
