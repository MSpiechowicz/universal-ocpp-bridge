//! Collector behaviour on an in-memory route and paused time: deadline, teardown and capacity.
use super::*;
use crate::call::reports::{
    ReportKind, RouteContext, SharedReports, disconnect, register, registry, route,
};
use crate::multipart::{ReportKey, reserve_report};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle, time::Instant};
use uob_application::RuntimeResourceLimits;
use uob_contracts::{
    BridgeId, CorrelationId, DeviceReportFailure201, MonitoringQuery201, ProtocolActionName,
    ProtocolEdition, ResourceRef, StationId,
};

struct Open {
    reports: SharedReports,
    budget: RuntimeResourceBudget,
    started: Option<oneshot::Sender<Instant>>,
    seen: Arc<AtomicBool>,
    collecting: JoinHandle<Collected>,
}

fn open(budget: RuntimeResourceBudget) -> Open {
    let reports = registry(&budget);
    let key = ReportKey {
        station: StationId::new("station").unwrap(),
        connection: CorrelationId::new("connection").unwrap(),
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("NotifyMonitoringReport").unwrap(),
        request_id: 5,
        correlation: CorrelationId::new("correlation").unwrap(),
    };
    let resource = ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let query = MonitoringQuery201::GetMonitoringReport {
        request_id: 5,
        criteria: vec![],
        component_variables: vec![],
    };
    let admission = reserve_report(key.clone(), ReportLimits::default(), &budget).unwrap();
    let (report_route, started) = register(
        &reports,
        key,
        resource,
        RouteContext::Monitoring(query),
        admission,
    )
    .unwrap();
    let seen = Arc::new(AtomicBool::new(false));
    let collecting = tokio::spawn(collect(report_route, budget.clone(), 64, seen.clone()));

    Open {
        reports,
        budget,
        started: Some(started),
        seen,
        collecting,
    }
}

impl Open {
    /// The call was written to the socket.
    fn dispatched(&mut self) {
        self.started.take().unwrap().send(Instant::now()).unwrap();
    }
}

fn fragment_payload(sequence: u32, more: bool) -> Value {
    json!({
        "requestId": 5, "seqNo": sequence, "tbc": more, "generatedAt": "2026-11-10T09:15:00Z",
        "monitor": [{
            "component": {"name": "A"}, "variable": {"name": "B"},
            "variableMonitoring": [{"id": sequence, "transaction": false, "value": 1, "type": "Delta", "severity": 5}],
        }],
    })
}

fn deliver(open: &Open, payload: &Value) -> Option<bool> {
    route(
        &open.reports,
        ReportKind::Monitoring,
        payload,
        512,
        &open.budget,
    )
}

fn reason(collected: &Collected) -> (DeviceReportFailure201, Option<u32>) {
    let MonitoringReportState201::Incomplete { reason, progress } = &collected.report else {
        panic!("incomplete report, got {:?}", collected.report);
    };
    (*reason, progress.map(|progress| progress.fragments))
}

/// Lets the collector task consume what the route queued.
async fn settle() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn the_deadline_is_absolute_from_dispatch_and_fragments_never_extend_it() {
    let mut open = open(budget());
    open.dispatched();
    assert_eq!(deliver(&open, &fragment_payload(0, true)), Some(true));
    settle().await;
    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(deliver(&open, &fragment_payload(1, true)), Some(true));
    settle().await;
    tokio::time::advance(Duration::from_secs(2)).await;

    let collected = open.collecting.await.unwrap();
    assert_eq!(
        reason(&collected),
        (DeviceReportFailure201::Timeout, Some(2))
    );
    assert!(collected.retained.is_none());
    assert!(open.seen.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn the_clock_starts_only_when_the_call_was_dispatched() {
    let mut open = open(budget());
    // Waiting for dispatch is not time spent collecting.
    tokio::time::advance(Duration::from_secs(120)).await;
    assert!(!open.collecting.is_finished());
    open.started.take();

    let collected = open.collecting.await.unwrap();
    assert_eq!(reason(&collected).0, DeviceReportFailure201::NotTransmitted);
}

#[tokio::test(start_paused = true)]
async fn teardown_mid_report_reports_disconnected_with_progress() {
    let mut open = open(budget());
    open.dispatched();
    assert_eq!(deliver(&open, &fragment_payload(0, true)), Some(true));
    settle().await;
    disconnect(&open.reports);

    let collected = open.collecting.await.unwrap();
    assert_eq!(
        reason(&collected),
        (DeviceReportFailure201::Disconnected, Some(1))
    );
}

#[tokio::test(start_paused = true)]
async fn a_complete_report_holds_its_reservations_until_dropped() {
    let shared = budget();
    let mut open = open(shared.clone());
    open.dispatched();
    assert_eq!(deliver(&open, &fragment_payload(0, false)), Some(true));

    let collected = open.collecting.await.unwrap();
    let MonitoringReportState201::Complete { monitors, .. } = &collected.report else {
        panic!("complete report");
    };
    assert_eq!(monitors.len(), 1);
    let held = shared.snapshot().queued_payload_bytes;
    assert!(held > 0);
    drop(collected);
    drop(open.reports);
    assert!(shared.snapshot().queued_payload_bytes < held);
}

#[tokio::test(start_paused = true)]
async fn an_exhausted_shared_budget_fails_the_report_with_capacity() {
    let mut open = open(budget());
    open.dispatched();
    // A fragment whose worst-case expansion cannot be reserved is acknowledged and fails the
    // report; the connection itself carries on.
    let oversized = route(
        &open.reports,
        ReportKind::Monitoring,
        &fragment_payload(0, true),
        usize::MAX / 16,
        &open.budget,
    );
    assert_eq!(oversized, Some(true));

    let collected = open.collecting.await.unwrap();
    assert_eq!(reason(&collected).0, DeviceReportFailure201::Capacity);
}

#[tokio::test(start_paused = true)]
async fn the_result_budget_is_reserved_before_a_complete_report_is_built() {
    // Room for the route and its descriptors, but not for the 3 MiB result expansion.
    let tight = RuntimeResourceBudget::new(RuntimeResourceLimits {
        aggregate_queued_payload_bytes: 1024 * 1024,
        reserved_critical_payload_bytes: 64 * 1024,
        trace_ring_bytes: 64 * 1024,
        ..RuntimeResourceLimits::default()
    })
    .unwrap();
    let mut open = open(tight);
    open.dispatched();
    assert_eq!(deliver(&open, &fragment_payload(0, false)), Some(true));

    let collected = open.collecting.await.unwrap();
    // The fragment was accepted; only the result expansion did not fit.
    assert_eq!(
        reason(&collected),
        (DeviceReportFailure201::Capacity, Some(1))
    );
}
