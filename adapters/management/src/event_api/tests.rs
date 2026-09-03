use std::{
    collections::BTreeSet,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};
use uob_application::{
    RetainedEventCursor, RetainedEventItem, RuntimeResourceBudget, RuntimeResourceLimits,
    TargetPortError, TargetRetainedEventStream, TargetSubscription, WorkClass,
};
use uob_contracts::{BridgeId, ResourceRef, StationId};

use super::{ManagementEventLimits, SubscriberBudget, matches_event_type, pump, safe_sse_id};

#[test]
fn source_cursors_cannot_inject_sse_fields() {
    let cursor = RetainedEventCursor::new("uob:event:1\nid: forged").unwrap();
    assert_eq!(safe_sse_id(&cursor), None);
}

#[test]
fn event_type_filter_is_exact_and_empty_means_all() {
    assert!(matches_event_type(&BTreeSet::new(), "station.changed.v1"));
    let filters = BTreeSet::from(["transaction.started.v1".to_owned()]);
    assert!(matches_event_type(&filters, "transaction.started.v1"));
    assert!(!matches_event_type(&filters, "station.changed.v1"));
}

#[tokio::test]
async fn disconnected_idle_client_releases_admission() {
    let resources = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let reservation = resources.try_reserve(WorkClass::Subscriber, 0).unwrap();
    let subscriber_budget = Arc::new(SubscriberBudget::new(reservation));
    let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
    let (sender, receiver) = mpsc::channel(1);
    let task = tokio::spawn(pump(
        Box::pin(PendingSubscription) as TargetRetainedEventStream<Value>,
        sender,
        station_resource(),
        BTreeSet::new(),
        ManagementEventLimits::default(),
        permit,
        subscriber_budget,
    ));

    tokio::task::yield_now().await;
    drop(receiver);
    tokio::time::timeout(Duration::from_millis(100), task)
        .await
        .expect("disconnect must cancel a pending source poll")
        .unwrap();
    assert_eq!(resources.snapshot().queues.subscribers, 0);
}

struct PendingSubscription;

impl TargetSubscription<Value> for PendingSubscription {
    fn poll_event(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<Value>, TargetPortError>>> {
        Poll::Pending
    }

    fn capacity(&self) -> usize {
        1
    }

    fn backlog(&self) -> usize {
        0
    }
}

fn station_resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        station_id: StationId::new("station-a").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
