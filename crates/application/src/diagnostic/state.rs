//! Bounded safe state projections for diagnostic comparison only.
use crate::{FlowEvidence, FlowSpan, FlowStage, SafeDiagnosticField};
use uob_contracts::{AvailabilityState, StationSnapshot};
/// At most 16 canonical resource availability values, never a station snapshot or vendor payload.
#[derive(Clone, Debug)]
pub struct DiagnosticState {
    resources: Vec<AvailabilityState>,
    omitted: bool,
}
impl DiagnosticState {
    /// Takes a bounded projection in the station owner's stable resource order.
    #[must_use]
    pub fn capture(snapshot: &StationSnapshot) -> Self {
        Self {
            resources: snapshot
                .resources
                .iter()
                .take(16)
                .map(|r| r.availability)
                .collect(),
            omitted: snapshot.resources.len() > 16,
        }
    }
    /// Emits only changed fields with before/after enum values and a truncation indicator.
    /// The caller must retain topology order for this single ordered operation.
    pub fn emit_changes(&self, after: &StationSnapshot, trace: &FlowSpan) {
        let changes = self
            .resources
            .iter()
            .zip(after.resources.iter())
            .enumerate()
            .filter(|(_, (before, after))| **before != after.availability)
            .map(
                |(index, (before, after))| SafeDiagnosticField::AvailabilityChange {
                    index,
                    before: *before,
                    after: after.availability,
                },
            )
            .collect::<Vec<_>>();
        if !changes.is_empty() {
            trace.emit_fields(FlowStage::StateChange, FlowEvidence::Completed, changes);
        }
        if self.omitted || after.resources.len() != self.resources.len() {
            trace.emit_fields(
                FlowStage::StateChange,
                FlowEvidence::Completed,
                vec![SafeDiagnosticField::StateDetailsOmitted],
            );
        }
    }
}
