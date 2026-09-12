use serde_json::Value;
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};
use uob_application::{
    CanonicalQuerySource, Page, RetainedEventCursor, RetainedEventItem, RetainedEventQuery,
    SnapshotCursor, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryAuthorization, TargetQueryResult, TargetRetainedEventStream, TargetSubscription,
};
use uob_contracts::{
    BridgeId, Connectivity, ContractVersion, EventEnvelope, EventId, EventOrigin, EventType,
    ResourceCapabilities, ResourceRef, RuntimeIdentity, StationId, StationSnapshot,
};

pub struct Source {
    pub runtime: RuntimeIdentity,
}
impl CanonicalQuerySource<Value> for Source {
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<Value>> {
        Box::pin(async move {
            match query {
                TargetQuery::StationSnapshots(_) => Ok(TargetQueryResult::StationSnapshots(Page {
                    items: if authorization.permits_resource(&resource()) {
                        vec![snapshot()]
                    } else {
                        vec![]
                    },
                    next_cursor: None::<SnapshotCursor>,
                })),
                TargetQuery::StationSnapshot(selected) => Ok(TargetQueryResult::StationSnapshot(
                    (selected == resource() && authorization.permits_resource(&selected))
                        .then(snapshot),
                )),
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "fixture.unsupported",
                )),
            }
        })
    }
    fn subscribe_retained_events<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<Value>> {
        Box::pin(async move {
            let cursor = query.after.as_ref().map(RetainedEventCursor::as_str);
            let sequence = if cursor.is_none() { 1 } else { 2 };
            let items = if cursor == Some("uob:event:2") {
                VecDeque::new()
            } else {
                VecDeque::from([RetainedEventItem {
                    cursor: RetainedEventCursor::new(format!("uob:event:{sequence}")).unwrap(),
                    event: EventEnvelope {
                        event_id: EventId::new(format!("event-{sequence}")).unwrap(),
                        schema_version: ContractVersion::V1_INITIAL,
                        runtime: self.runtime.clone(),
                        resource: resource(),
                        source_time: None,
                        observed_at: serde_json::from_str("\"2026-09-12T00:00:00Z\"").unwrap(),
                        event_type: EventType::new("<img src=x onerror=alert(1)>").unwrap(),
                        origin: EventOrigin::Station,
                        sequence,
                        correlation_id: None,
                        causation_id: None,
                        provenance: None,
                        payload: serde_json::json!({"untrusted": "<script>alert(1)</script>"}),
                    },
                }])
            };
            // Initial EOF forces a real SSE disconnect; resumed subscriptions stay open.
            Ok(Box::pin(Subscription {
                items,
                close: cursor.is_none(),
            }) as TargetRetainedEventStream<Value>)
        })
    }
}
struct Subscription {
    items: VecDeque<RetainedEventItem<Value>>,
    close: bool,
}
impl TargetSubscription<Value> for Subscription {
    fn poll_event(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<Value>, TargetPortError>>> {
        let this = self.get_mut();
        match this.items.pop_front() {
            Some(item) => Poll::Ready(Some(Ok(item))),
            None if this.close => Poll::Ready(None),
            None => Poll::Pending,
        }
    }
    fn capacity(&self) -> usize {
        2
    }
    fn backlog(&self) -> usize {
        self.items.len()
    }
}
pub fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-browser-fixture").unwrap(),
        station_id: StationId::new("station-fixture").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
fn snapshot() -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: resource(),
        observed_at: serde_json::from_str("\"2026-09-12T00:00:00Z\"").unwrap(),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        transactions: vec![],
        current_values: vec![],
    }
}
