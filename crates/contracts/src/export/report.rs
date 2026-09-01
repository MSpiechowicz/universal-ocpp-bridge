use std::{collections::BTreeSet, error::Error, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ExportBatch, ExportBatchId, ExportDestination, ExportErrorCode, ExportRecordIdentity};

/// Transmission evidence that must never be mistaken for a confirmed remote commit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportUncertainStage {
    /// The provider wrote bytes but has no remote commit evidence.
    BytesWritten,
    /// A commit was requested, but its remote outcome could not be determined.
    CommitOutcomeUnknown,
}

/// Per-record result required when a provider cannot commit the batch atomically.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExportRecordOutcomeKind {
    /// The remote destination confirmed commit of this exact record.
    Committed,
    /// The record failed without a known remote commit and may be retried.
    Retryable { error: ExportErrorCode },
    /// The record was permanently rejected without a remote commit.
    Permanent { error: ExportErrorCode },
    /// Remote commit is unknown and blind retry may require deduplication.
    Uncertain {
        stage: ExportUncertainStage,
        error: ExportErrorCode,
    },
}

/// Explicit outcome for one identity in a non-atomic batch.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRecordOutcome {
    /// Exact root/subrecord identity being classified.
    pub identity: ExportRecordIdentity,
    /// Confirmed or unconfirmed per-record result.
    pub result: ExportRecordOutcomeKind,
}

/// Provider report for a whole batch.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExportOutcome {
    /// The remote destination confirmed one atomic commit for every record.
    Committed,
    /// No remote commit is known and the complete batch may be retried.
    Retryable { error: ExportErrorCode },
    /// No remote commit is known and the complete batch is permanently rejected.
    Permanent { error: ExportErrorCode },
    /// The complete batch has an unknown remote commit outcome.
    Uncertain {
        stage: ExportUncertainStage,
        error: ExportErrorCode,
    },
    /// Complete per-record classification from a non-atomic provider.
    Partial { records: Vec<ExportRecordOutcome> },
}

/// Validated provider report bound to the exact batch and destination revision.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(try_from = "UncheckedExportReport")]
pub struct ExportReport {
    batch_id: ExportBatchId,
    destination: ExportDestination,
    record_ids: Vec<ExportRecordIdentity>,
    outcome: ExportOutcome,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UncheckedExportReport {
    batch_id: ExportBatchId,
    destination: ExportDestination,
    record_ids: Vec<ExportRecordIdentity>,
    outcome: ExportOutcome,
}

impl TryFrom<UncheckedExportReport> for ExportReport {
    type Error = ExportReportError;

    fn try_from(value: UncheckedExportReport) -> Result<Self, Self::Error> {
        Self::validate(
            value.batch_id,
            value.destination,
            value.record_ids,
            value.outcome,
        )
    }
}

impl ExportReport {
    /// Creates a report bound to a batch and validates any partial classification.
    ///
    /// # Errors
    ///
    /// Returns [`ExportReportError`] when a partial result is missing, duplicates, or invents a
    /// record identity.
    pub fn for_batch(
        batch: &ExportBatch,
        outcome: ExportOutcome,
    ) -> Result<Self, ExportReportError> {
        Self::validate(
            batch.batch_id.clone(),
            batch.destination.clone(),
            batch
                .records
                .iter()
                .map(|record| record.metadata.identity.clone())
                .collect(),
            outcome,
        )
    }

    fn validate(
        batch_id: ExportBatchId,
        destination: ExportDestination,
        record_ids: Vec<ExportRecordIdentity>,
        outcome: ExportOutcome,
    ) -> Result<Self, ExportReportError> {
        let expected = record_ids.iter().collect::<BTreeSet<_>>();
        if expected.is_empty() {
            return Err(ExportReportError::EmptyRecordSet);
        }
        if expected.len() != record_ids.len() {
            return Err(ExportReportError::DuplicateRecordIdentity);
        }
        if let ExportOutcome::Partial { records } = &outcome {
            let actual = records
                .iter()
                .map(|record| &record.identity)
                .collect::<BTreeSet<_>>();
            if actual.len() != records.len() {
                return Err(ExportReportError::DuplicatePartialIdentity);
            }
            if expected != actual {
                return Err(ExportReportError::IncompletePartialClassification);
            }
        }
        Ok(Self {
            batch_id,
            destination,
            record_ids,
            outcome,
        })
    }

    /// Returns the stable batch identity supplied by the host.
    #[must_use]
    pub const fn batch_id(&self) -> &ExportBatchId {
        &self.batch_id
    }

    /// Returns the destination and immutable configuration revision from the batch.
    #[must_use]
    pub const fn destination(&self) -> &ExportDestination {
        &self.destination
    }

    /// Returns every root/subrecord identity classified by this report.
    #[must_use]
    pub fn record_ids(&self) -> &[ExportRecordIdentity] {
        &self.record_ids
    }

    /// Returns true only when a confirmed atomic remote commit covers the full batch.
    #[must_use]
    pub const fn is_fully_committed(&self) -> bool {
        matches!(self.outcome, ExportOutcome::Committed)
    }

    /// Returns only record identities with explicit remote-commit confirmation.
    ///
    /// Bytes written, unknown commit outcomes, and every unconfirmed failure return no checkpoint
    /// evidence. A partial provider returns only its explicitly committed records.
    pub fn confirmed_record_ids(&self) -> impl Iterator<Item = &ExportRecordIdentity> {
        self.record_ids
            .iter()
            .filter(|identity| match &self.outcome {
                ExportOutcome::Committed => true,
                ExportOutcome::Partial { records } => records.iter().any(|record| {
                    record.identity == **identity
                        && matches!(record.result, ExportRecordOutcomeKind::Committed)
                }),
                ExportOutcome::Retryable { .. }
                | ExportOutcome::Permanent { .. }
                | ExportOutcome::Uncertain { .. } => false,
            })
    }

    /// Returns the validated provider outcome.
    #[must_use]
    pub const fn outcome(&self) -> &ExportOutcome {
        &self.outcome
    }
}

/// Invalid non-atomic report that cannot safely drive retry or checkpoint state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportReportError {
    /// A report cannot classify zero records.
    EmptyRecordSet,
    /// The report's batch record list repeats an identity.
    DuplicateRecordIdentity,
    /// A record identity appeared more than once in the partial result.
    DuplicatePartialIdentity,
    /// Partial results did not classify every and only record in the batch.
    IncompletePartialClassification,
}

impl fmt::Display for ExportReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRecordSet => formatter.write_str("export report cannot be empty"),
            Self::DuplicateRecordIdentity => {
                formatter.write_str("export report repeats a batch record identity")
            }
            Self::DuplicatePartialIdentity => {
                formatter.write_str("partial export report repeats a record identity")
            }
            Self::IncompletePartialClassification => formatter.write_str(
                "partial export report must classify every and only record in the batch",
            ),
        }
    }
}

impl Error for ExportReportError {}
