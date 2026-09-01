use std::collections::BTreeMap;

use uob_application::{DeliveryId, DeliveryOutcome, DeliveryReport, TargetDelivery};

/// Host recovery action implied by a target's exact delivery outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDisposition {
    /// The durable item is complete and may leave the outbox.
    Complete,
    /// The item remains eligible for bounded retry after restart.
    Retryable,
    /// The item must be reconciled before any retry.
    Reconcile,
}

/// Small in-memory model of host-owned durable delivery recovery.
///
/// It intentionally lives outside target sessions: restarting an adapter cannot erase pending
/// work or turn an uncertain handoff into a blind retry.
pub struct DeliveryRecoveryLedger<E> {
    pending: BTreeMap<DeliveryId, TargetDelivery<E>>,
}

impl<E> Default for DeliveryRecoveryLedger<E> {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }
}

impl<E> DeliveryRecoveryLedger<E> {
    /// Records host-owned work before exposing it to a target session.
    pub fn record(&mut self, delivery: TargetDelivery<E>) {
        self.pending.insert(delivery.delivery_id.clone(), delivery);
    }

    /// Applies an exact report while preserving retry and reconciliation distinctions.
    #[must_use]
    pub fn apply(&mut self, report: &DeliveryReport) -> Option<RecoveryDisposition> {
        if !self.pending.contains_key(&report.delivery_id) {
            return None;
        }
        let disposition = match report.outcome {
            DeliveryOutcome::RetryableFailure { .. } => RecoveryDisposition::Retryable,
            DeliveryOutcome::Uncertain { .. } => RecoveryDisposition::Reconcile,
            DeliveryOutcome::LocallyExposed { .. }
            | DeliveryOutcome::Acknowledged { .. }
            | DeliveryOutcome::PermanentFailure { .. } => RecoveryDisposition::Complete,
        };
        if disposition == RecoveryDisposition::Complete {
            self.pending.remove(&report.delivery_id);
        }
        Some(disposition)
    }

    /// Returns pending work that may safely be offered to a restarted adapter.
    pub fn pending(&self) -> impl Iterator<Item = &TargetDelivery<E>> {
        self.pending.values()
    }
}
