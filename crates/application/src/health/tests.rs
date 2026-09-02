use super::*;
use crate::WorkClass;

fn monitor() -> HealthMonitor {
    HealthMonitor::new(RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap())
}

#[test]
fn external_outages_do_not_change_core_readiness() {
    let monitor = monitor();
    monitor.report_core_loop(CoreLoopState::Ready);
    monitor.report_storage(StorageHealthState::Safe, None);
    for kind in [
        ComponentKind::Target,
        ComponentKind::ExternalBroker,
        ComponentKind::ExternalClient,
        ComponentKind::ExternalExporter,
    ] {
        monitor.report_component(
            kind,
            ComponentHealth {
                state: ComponentHealthState::Reconnecting,
                reconnects: 2,
                backlog_items: 3,
                in_flight_items: 1,
                active_connections: 0,
                reason: Some("external.unavailable".into()),
            },
        );
    }
    let snapshot = monitor.snapshot();
    assert_eq!(snapshot.readiness, ReadinessState::Ready);
    assert!(snapshot.accepts_new_sessions);
    assert_eq!(snapshot.components.len(), 4);
}

#[test]
fn storage_pressure_preserves_core_but_refuses_new_sessions() {
    let monitor = monitor();
    monitor.report_core_loop(CoreLoopState::Ready);
    monitor.report_storage(StorageHealthState::CapacityProtected, None);
    let snapshot = monitor.snapshot();
    assert_eq!(snapshot.readiness, ReadinessState::Ready);
    assert!(!snapshot.accepts_new_sessions);
    monitor.report_storage(StorageHealthState::Failed, None);
    assert_eq!(monitor.snapshot().readiness, ReadinessState::NotReady);
}

#[test]
fn target_and_export_backlogs_remain_inside_daemon_accounting() {
    let monitor = monitor();
    let target = monitor
        .resources()
        .try_reserve(WorkClass::TargetRetry, 17)
        .unwrap();
    let exporter = monitor
        .resources()
        .try_reserve(WorkClass::ExporterBatch, 19)
        .unwrap();

    let snapshot = monitor.snapshot();
    assert_eq!(snapshot.runtime.queued_payload_bytes, 36);
    assert_eq!(snapshot.runtime.queues.target_retries, 1);
    assert_eq!(snapshot.runtime.queues.exporter_batches, 1);

    drop((target, exporter));
    assert_eq!(monitor.snapshot().runtime.queued_payload_bytes, 0);
}

#[test]
fn latency_and_process_metrics_are_bounded_and_separated() {
    let monitor = monitor();
    for latency in 1..=100 {
        monitor.record_latency(LatencyClass::LocalResponse, latency);
    }
    monitor.report_daemon_process(ProcessResourceMetrics {
        rss_bytes: 10,
        cpu_time_milliseconds: 20,
    });
    monitor.report_auxiliary_process(
        AuxiliaryProcess::Broker,
        ProcessResourceMetrics {
            rss_bytes: 30,
            cpu_time_milliseconds: 40,
        },
    );
    let snapshot = monitor.snapshot();
    assert_eq!(snapshot.local_response_latency.samples, 100);
    assert_eq!(
        snapshot.local_response_latency.p95_upper_bound_ms,
        Some(100)
    );
    assert_eq!(snapshot.daemon_process.unwrap().rss_bytes, 10);
    assert_eq!(
        snapshot.auxiliary_processes[&AuxiliaryProcess::Broker].rss_bytes,
        30
    );
}
