//! Passive deterministic health observations; no external provider is constructed.
use uob_application::{
    Application, ComponentHealth, ComponentHealthState, ComponentKind, CoreLoopState,
    ExportObservation, LatencyClass, SafeEndpointLabel, StorageHealthState,
};
use uob_contracts::ExportRecordKind;

pub fn configure(application: &Application) {
    let health = application.health();
    health.report_core_loop(CoreLoopState::Ready);
    health.report_storage(StorageHealthState::Safe, None);
    health.record_latency(LatencyClass::StorageOperation, 3);
    health.report_target_configuration(
        SafeEndpointLabel::new("ems-scada.http").unwrap(),
        vec![SafeEndpointLabel::new("canonical-read").unwrap()],
    );
    health.report_component(
        ComponentKind::ExternalExporter,
        ComponentHealth {
            state: ComponentHealthState::Reconnecting,
            reconnects: 2,
            backlog_items: 7,
            in_flight_items: 0,
            active_connections: 0,
            reason: Some("export.connection_unavailable".into()),
        },
    );
    health.report_export_observation(Some(ExportObservation {
        provider: SafeEndpointLabel::new("postgresql").unwrap(),
        destination_revision: SafeEndpointLabel::new("test-export-revision-1").unwrap(),
        record_classes: vec![ExportRecordKind::Measurement],
        enqueued_records: Some(24),
        remote_committed_records: Some(12),
        successful_batches: Some(2),
        failed_batches: Some(1),
        retries: Some(2),
        lag_milliseconds: Some(30000),
        duplicates: Some(0),
        quarantined_records: Some(3),
        gap_count: Some(1),
        dropped_records: Some(2),
    }));
}
