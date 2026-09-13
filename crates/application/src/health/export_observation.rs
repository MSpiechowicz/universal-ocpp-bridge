//! Passive exporter observations. Reporting never constructs or calls a database provider.
use crate::SafeEndpointLabel;
use uob_contracts::ExportRecordKind;

/// Bounded, credential-free exporter evidence supplied by its owning worker.
/// None means unobserved; zero is an explicitly observed count. Counts belong to
/// the supplied destination/revision label and must not be carried across rerouting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportObservation {
    pub provider: SafeEndpointLabel,
    pub destination_revision: SafeEndpointLabel,
    pub record_classes: Vec<ExportRecordKind>,
    pub enqueued_records: Option<u64>,
    pub remote_committed_records: Option<u64>,
    pub successful_batches: Option<u64>,
    pub failed_batches: Option<u64>,
    pub retries: Option<u64>,
    pub lag_milliseconds: Option<u64>,
    pub duplicates: Option<u64>,
    pub quarantined_records: Option<u64>,
    pub gap_count: Option<u64>,
    pub dropped_records: Option<u64>,
}

impl ExportObservation {
    /// Normalizes the closed record-class set before retaining it in the snapshot.
    pub(super) fn bounded(mut self) -> Self {
        self.record_classes = [
            ExportRecordKind::Measurement,
            ExportRecordKind::TransactionLifecycle,
            ExportRecordKind::ResourceStatusChange,
            ExportRecordKind::PointChange,
            ExportRecordKind::CommandResult,
        ]
        .into_iter()
        .filter(|kind| self.record_classes.contains(kind))
        .collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ComponentHealth, ComponentHealthState, ComponentKind, HealthMonitor, RuntimeResourceBudget,
        RuntimeResourceLimits,
    };

    #[test]
    fn passive_observations_are_bounded_nullable_and_cleared_when_disabled() {
        let monitor = HealthMonitor::new(
            RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap(),
        );
        let budget = monitor.resources().snapshot();
        assert!(monitor.snapshot().export_observation.is_none());
        monitor.report_export_observation(Some(ExportObservation {
            provider: SafeEndpointLabel::new("test-provider").unwrap(),
            destination_revision: SafeEndpointLabel::new("export-revision-2").unwrap(),
            record_classes: vec![ExportRecordKind::Measurement; 100],
            enqueued_records: Some(1),
            remote_committed_records: None,
            successful_batches: None,
            failed_batches: None,
            retries: None,
            lag_milliseconds: None,
            duplicates: Some(0),
            quarantined_records: Some(3),
            gap_count: Some(1),
            dropped_records: Some(2),
        }));
        let snapshot = monitor.snapshot();
        let observation = snapshot.export_observation.unwrap();
        assert_eq!(observation.record_classes.len(), 1);
        assert_eq!(observation.remote_committed_records, None);
        assert_eq!(observation.duplicates, Some(0));
        assert!(snapshot.export_observation_age_ms.is_some());
        assert_eq!(monitor.resources().snapshot(), budget);
        monitor.report_component(
            ComponentKind::ExternalExporter,
            ComponentHealth {
                state: ComponentHealthState::Disabled,
                reconnects: 0,
                backlog_items: 0,
                in_flight_items: 0,
                active_connections: 0,
                reason: None,
            },
        );
        assert!(monitor.snapshot().export_observation.is_none());
        assert!(monitor.snapshot().export_observation_age_ms.is_none());
        assert_eq!(monitor.resources().snapshot(), budget);
    }
}
