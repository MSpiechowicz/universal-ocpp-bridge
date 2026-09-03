use std::sync::Arc;

use serde::Serialize;
use time::OffsetDateTime;
use uob_application::{
    DeliveryId, DeliveryOutcome, DeliveryReport, TargetDiagnostic, TargetHealth, TargetHealthState,
};
use uob_contracts::UtcTimestamp;

use super::Session;

impl<E, P> Session<E, P>
where
    E: Serialize + Send + Sync + 'static,
    P: Send + 'static,
{
    pub(super) fn spawn_report(&mut self, delivery_id: DeliveryId, outcome: DeliveryOutcome) {
        let reports = Arc::clone(&self.context.critical_reports);
        self.reports.spawn(async move {
            reports
                .report(DeliveryReport {
                    delivery_id,
                    outcome,
                    reported_at: UtcTimestamp::new(OffsetDateTime::now_utc()),
                })
                .await
        });
    }

    pub(super) fn emit_health(&self, state: TargetHealthState, reason: &'static str) {
        let _ = self
            .context
            .diagnostics
            .try_emit(TargetDiagnostic::Health(TargetHealth {
                state,
                delivery_backlog: self.context.deliveries.backlog(),
                in_flight_deliveries: self.outstanding_delivery_count(),
                active_connections: usize::from(self.connected),
                reason: Some(reason.to_owned()),
            }));
    }
}
