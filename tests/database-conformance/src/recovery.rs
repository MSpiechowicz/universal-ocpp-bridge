use std::collections::BTreeMap;

use uob_contracts::{ExportOutcome, ExportRecordIdentity, ExportReport};

/// Host recovery action derived only from explicit provider evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryDisposition {
    /// Every record has confirmed commit evidence and may advance its checkpoint.
    Confirmed(Vec<ExportRecordIdentity>),
    /// No commit is known and the complete batch may be retried.
    Retry,
    /// No commit is known and the batch is quarantined.
    Quarantine,
    /// Remote side effects are unknown; reconciliation must precede retry.
    Reconcile,
    /// A per-record provider supplied mixed explicit outcomes.
    Partial(Vec<ExportRecordIdentity>),
}

/// Small deterministic ledger used by outage and at-least-once adapter tests.
#[derive(Default)]
pub struct ExportRecoveryLedger {
    dispositions: BTreeMap<String, RecoveryDisposition>,
}

impl ExportRecoveryLedger {
    /// Records a report without interpreting transport handoff as a commit.
    pub fn apply(&mut self, report: &ExportReport) -> RecoveryDisposition {
        let confirmed = report.confirmed_record_ids().cloned().collect::<Vec<_>>();
        let disposition = match report.outcome() {
            ExportOutcome::Committed => RecoveryDisposition::Confirmed(confirmed),
            ExportOutcome::Retryable { .. } => RecoveryDisposition::Retry,
            ExportOutcome::Permanent { .. } => RecoveryDisposition::Quarantine,
            ExportOutcome::Uncertain { .. } => RecoveryDisposition::Reconcile,
            ExportOutcome::Partial { .. } => RecoveryDisposition::Partial(confirmed),
        };
        self.dispositions
            .insert(report.batch_id().as_str().to_owned(), disposition.clone());
        disposition
    }

    /// Returns the latest action for a stable batch identity.
    #[must_use]
    pub fn disposition(&self, batch_id: &str) -> Option<&RecoveryDisposition> {
        self.dispositions.get(batch_id)
    }
}
