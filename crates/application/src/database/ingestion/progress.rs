use crate::{CommittedRecordCursor, CommittedRecordDescriptor, CommittedRecordPage, Durability};

use super::super::spool::{
    ExportGap, ExportGapReason, ExportSourceCheckpoint, ExportSpoolGapCommit, ExportSpoolNamespace,
    ExportSpoolRecordBegin, ExportSpoolStatus,
};
use super::ExportIngestionError;

pub(super) fn prepare_progress(
    namespace: ExportSpoolNamespace,
    durability: Durability,
    status: &ExportSpoolStatus,
    page: CommittedRecordPage,
) -> Result<(ExportSpoolGapCommit, Option<CommittedRecordDescriptor>), ExportIngestionError> {
    let expected = stream_checkpoint(status, durability).cloned();
    let previous = expected.as_ref().map_or(0, |point| point.sequence);
    let next = page
        .resume_cursor
        .as_str()
        .rsplit_once(':')
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .ok_or(ExportIngestionError::InvalidSourceCursor)?;
    let descriptor = page.items.into_iter().next();
    let mut gaps = Vec::new();
    let covered = if let Some(item) = &descriptor {
        if item.durability != durability || item.sequence <= previous || item.sequence > next {
            return Err(ExportIngestionError::InvalidSourceCursor);
        }
        if item.sequence > previous + 1 {
            gaps.push(ExportGap {
                durability,
                first: previous + 1,
                last: item.sequence - 1,
                reason: ExportGapReason::SourceExpired,
            });
        }
        item.sequence
    } else {
        previous
    };
    if next > covered {
        gaps.push(ExportGap {
            durability,
            first: covered + 1,
            last: next,
            reason: ExportGapReason::SourceExpired,
        });
    }
    let missing: u64 = gaps.iter().map(ExportGap::count).sum();
    if missing != page.lost_records || next < previous || next > page.high_water {
        return Err(ExportIngestionError::InvalidSourceCursor);
    }
    if let Some(item) = &descriptor {
        gaps.retain(|gap| gap.last < item.sequence);
    }
    Ok((
        ExportSpoolGapCommit {
            namespace,
            durability,
            expected,
            next: ExportSourceCheckpoint {
                cursor: page.resume_cursor,
                sequence: next,
            },
            high_water: page.high_water,
            gaps,
            legacy_baseline_incomplete: page.legacy_baseline_incomplete,
        },
        descriptor,
    ))
}

pub(super) fn leading_gap_commit(
    request: &ExportSpoolGapCommit,
    descriptor: &CommittedRecordDescriptor,
) -> Result<ExportSpoolGapCommit, ExportIngestionError> {
    let last = descriptor.sequence - 1;
    let prefix = descriptor
        .cursor
        .as_str()
        .rsplit_once(':')
        .ok_or(ExportIngestionError::InvalidSourceCursor)?
        .0;
    let cursor = CommittedRecordCursor::new(format!("{prefix}:{last}"))
        .map_err(|_| ExportIngestionError::InvalidSourceCursor)?;
    Ok(ExportSpoolGapCommit {
        namespace: request.namespace.clone(),
        durability: request.durability,
        expected: request.expected.clone(),
        next: ExportSourceCheckpoint {
            cursor,
            sequence: last,
        },
        high_water: request.high_water,
        gaps: request.gaps.clone(),
        legacy_baseline_incomplete: request.legacy_baseline_incomplete,
    })
}

pub(super) fn record_begin(
    request: &ExportSpoolGapCommit,
    status: &ExportSpoolStatus,
    descriptor: &CommittedRecordDescriptor,
) -> ExportSpoolRecordBegin {
    ExportSpoolRecordBegin {
        progress: ExportSpoolGapCommit {
            namespace: request.namespace.clone(),
            durability: request.durability,
            expected: stream_checkpoint(status, request.durability).cloned(),
            next: ExportSourceCheckpoint {
                cursor: descriptor.cursor.clone(),
                sequence: descriptor.sequence,
            },
            high_water: request.high_water,
            gaps: Vec::new(),
            legacy_baseline_incomplete: request.legacy_baseline_incomplete,
        },
        descriptor: descriptor.clone(),
    }
}

pub(super) fn stream_checkpoint(
    status: &ExportSpoolStatus,
    durability: Durability,
) -> Option<&ExportSourceCheckpoint> {
    match durability {
        Durability::Critical => status.critical.as_ref(),
        Durability::BestEffortTelemetry => status.telemetry.as_ref(),
    }
}

pub(super) fn status_high_water(status: &ExportSpoolStatus, durability: Durability) -> u64 {
    match durability {
        Durability::Critical => status.critical_high_water,
        Durability::BestEffortTelemetry => status.telemetry_high_water,
    }
}
