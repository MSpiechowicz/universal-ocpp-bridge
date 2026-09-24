use super::{Host, query::Store};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};
use uob_application::{
    DeliveryAttempt, DeliveryAttemptResolution, DeliveryId, DeliveryOutcome, DeliveryReport,
    OperationalStore, PageLimit, PendingDeliveryQuery, RetainedEventQuery, SnapshotQuery,
    TargetDelivery, TargetDeliveryClass, TargetDeliveryStore, TargetMessage,
};
use uob_contracts::{StationEvent, StationSnapshot, TargetInstanceId, UtcTimestamp};

fn ready_at() -> UtcTimestamp {
    serde_json::from_str("\"2026-09-01T03:00:00Z\"").unwrap()
}

pub struct Pump {
    pub task: JoinHandle<()>,
    pub observed: mpsc::Receiver<DeliveryReport>,
    deferred: VecDeque<DeliveryReport>,
}
impl Pump {
    pub fn start(host: &mut Host) -> Self {
        let store = host.store.clone();
        let deliveries = host.deliveries.clone();
        let (unused_tx, replacement) = mpsc::channel(1);
        drop(unused_tx);
        let mut reports = std::mem::replace(&mut host.reports, replacement);
        let (observed_tx, observed) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            let mut previous = BTreeMap::<String, StationSnapshot>::new();
            let mut pending = BTreeSet::<String>::new();
            let mut sequence = 0_u64;
            loop {
                while let Ok(report) = reports.try_recv() {
                    if pending.remove(report.delivery_id.as_str()) {
                        assert!(
                            matches!(report.outcome, DeliveryOutcome::Acknowledged { ref scope, .. }
                            if scope.0 == "mqtt.broker_received"),
                            "durable broker report: {:?}",
                            report.outcome
                        );
                        store
                            .record_delivery_attempt(DeliveryAttempt {
                                report: report.clone(),
                                resolution: DeliveryAttemptResolution::Final,
                            })
                            .await
                            .unwrap();
                    }
                    if observed_tx.send(report).await.is_err() {
                        return;
                    }
                }
                let page = store
                    .read_snapshots(SnapshotQuery {
                        after: None,
                        limit: PageLimit::new(8).unwrap(),
                    })
                    .await
                    .unwrap();
                for snapshot in page.items {
                    let station = snapshot.station.station_id.as_str().to_owned();
                    if previous.get(&station) == Some(&snapshot) {
                        continue;
                    }
                    sequence += 1;
                    previous.insert(station.clone(), snapshot.clone());
                    deliveries
                        .send(TargetDelivery {
                            delivery_id: DeliveryId::new(format!("snapshot/{station}/{sequence}"))
                                .unwrap(),
                            target_instance_id: TargetInstanceId::new("main").unwrap(),
                            target_configuration_revision: 1,
                            station_ordering_key: snapshot.station.clone(),
                            deadline: ready_at(),
                            class: TargetDeliveryClass::ReplaceableLatestState,
                            message: Arc::new(TargetMessage::StationSnapshot(snapshot)),
                        })
                        .await
                        .unwrap();
                }
                dispatch_pending(&store, &deliveries, &mut pending).await;
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        });
        Self {
            task,
            observed,
            deferred: VecDeque::new(),
        }
    }

    /// Wait for both boot snapshots without discarding other broker reports.
    pub async fn expect_initial_snapshots(&mut self) {
        let mut missing = BTreeSet::from(["station-a", "station-b"]);
        tokio::time::timeout(Duration::from_secs(12), async {
            while !missing.is_empty() {
                let report = self
                    .observed
                    .recv()
                    .await
                    .expect("broker report channel closed");
                let id = report.delivery_id.as_str();
                let station = id
                    .strip_prefix("snapshot/")
                    .and_then(|suffix| suffix.split_once('/'))
                    .and_then(|(station, sequence)| {
                        (!sequence.is_empty() && missing.contains(station)).then_some(station)
                    });
                if let Some(station) = station {
                    assert!(
                        matches!(report.outcome, DeliveryOutcome::Acknowledged { ref scope, .. }
                        if scope.0 == "mqtt.broker_received"),
                        "{station} boot snapshot broker report: {:?}",
                        report.outcome
                    );
                    missing.remove(station);
                } else {
                    self.deferred.push_back(report);
                }
            }
        })
        .await
        .expect("both boot snapshots broker acknowledged");
    }

    pub async fn expect_reports(&mut self, count: usize) -> Vec<DeliveryReport> {
        let mut reports = Vec::new();
        for _ in 0..count {
            let report = if let Some(report) = self.deferred.pop_front() {
                report
            } else {
                tokio::time::timeout(Duration::from_secs(15), self.observed.recv())
                    .await
                    .expect("broker report deadline")
                    .expect("broker report channel closed")
            };
            assert!(
                matches!(report.outcome, DeliveryOutcome::Acknowledged { ref scope, .. }
                if scope.0 == "mqtt.broker_received"),
                "broker report: {:?}",
                report.outcome
            );
            reports.push(report);
        }
        reports
    }
}
impl Drop for Pump {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn dispatch_pending(
    store: &Store,
    deliveries: &mpsc::Sender<TargetDelivery<StationEvent>>,
    pending: &mut BTreeSet<String>,
) {
    let entries = store
        .read_pending_deliveries(PendingDeliveryQuery {
            target_instance_id: TargetInstanceId::new("main").unwrap(),
            target_configuration_revision: 1,
            ready_at: ready_at(),
            limit: PageLimit::new(8).unwrap(),
        })
        .await
        .unwrap();
    for entry in entries {
        let delivery = entry.delivery;
        if !pending.insert(delivery.delivery_id.as_str().to_owned()) {
            continue;
        }
        let page = store
            .read_retained_events(RetainedEventQuery {
                resource: delivery.ordering_key.clone(),
                after: None,
                limit: PageLimit::new(100).unwrap(),
            })
            .await
            .unwrap();
        let event = page
            .events
            .into_iter()
            .find(|event| event.event_id == delivery.event_id)
            .expect("durably committed outbox event");
        deliveries
            .send(TargetDelivery {
                delivery_id: delivery.delivery_id,
                target_instance_id: delivery.target_instance_id,
                target_configuration_revision: delivery.target_configuration_revision,
                station_ordering_key: delivery.ordering_key,
                deadline: delivery.deadline,
                class: TargetDeliveryClass::Durable,
                message: Arc::new(TargetMessage::DomainEvent(event)),
            })
            .await
            .unwrap();
    }
}
