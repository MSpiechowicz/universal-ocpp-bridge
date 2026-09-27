use rusqlite::{Connection, OptionalExtension, params};
use uob_application::{
    Durability, ExportGap, ExportGapReason, ExportSpoolError, ExportSpoolErrorCode,
};

use super::{Limits, fail, store::nonnegative};

pub(super) fn stream(value: Durability) -> i64 {
    match value {
        Durability::Critical => 0,
        Durability::BestEffortTelemetry => 1,
    }
}

pub(super) fn reason(value: ExportGapReason) -> i64 {
    match value {
        ExportGapReason::SourceExpired => 0,
        ExportGapReason::TelemetryDropped => 1,
        ExportGapReason::TelemetryEvicted => 2,
    }
}

pub(super) fn decode_reason(value: i64) -> Result<ExportGapReason, ExportSpoolError> {
    match value {
        0 => Ok(ExportGapReason::SourceExpired),
        1 => Ok(ExportGapReason::TelemetryDropped),
        2 => Ok(ExportGapReason::TelemetryEvicted),
        _ => Err(ExportSpoolError::new(
            ExportSpoolErrorCode::IntegrityFailure,
            "invalid durable export gap reason",
        )),
    }
}

pub(super) fn insert_gap(
    tx: &Connection,
    gap: &ExportGap,
    limits: Limits,
) -> Result<(), ExportSpoolError> {
    let first = i64::try_from(gap.first).map_err(|_| invalid_gap())?;
    let last = i64::try_from(gap.last).map_err(|_| invalid_gap())?;
    if first == 0 || last < first {
        return Err(invalid_gap());
    }
    let durability = stream(gap.durability);
    let kind = reason(gap.reason);
    let overlaps: i64 = tx
        .query_row(
            "SELECT count(*) FROM gaps WHERE durability=?1 AND first<=?2 AND last>=?3",
            params![durability, last, first],
            |row| row.get(0),
        )
        .map_err(|error| fail(&error))?;
    if overlaps != 0 {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::CheckpointConflict,
            "overlapping committed export gaps",
        ));
    }
    let lower: Option<(i64, i64)> = tx
        .query_row(
            "SELECT first, reason FROM gaps WHERE durability=?1 AND last=?2",
            params![durability, first - 1],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| fail(&error))?;
    let next = last.checked_add(1);
    let upper: Option<(i64, i64)> = next
        .map(|position| {
            tx.query_row(
                "SELECT last, reason FROM gaps WHERE durability=?1 AND first=?2",
                params![durability, position],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| fail(&error))
        })
        .transpose()?
        .flatten();
    let combined_first = if let Some((lower_first, lower_kind)) = lower {
        if lower_kind == kind {
            tx.execute(
                "DELETE FROM gaps WHERE durability=?1 AND first=?2",
                params![durability, lower_first],
            )
            .map_err(|error| fail(&error))?;
            lower_first
        } else {
            first
        }
    } else {
        first
    };
    let combined_last = if let Some((upper_last, upper_kind)) = upper {
        if upper_kind == kind {
            tx.execute(
                "DELETE FROM gaps WHERE durability=?1 AND first=?2",
                params![
                    durability,
                    next.expect("adjacent upper gap has a following position")
                ],
            )
            .map_err(|error| fail(&error))?;
            upper_last
        } else {
            last
        }
    } else {
        last
    };
    check_gap_capacity(tx, kind, limits)?;
    tx.execute(
        "INSERT INTO gaps(durability, first, last, reason) VALUES(?1,?2,?3,?4)",
        params![durability, combined_first, combined_last, kind],
    )
    .map_err(|error| fail(&error))?;
    Ok(())
}

fn invalid_gap() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::InvalidRequest,
        "invalid export gap interval",
    )
}

fn check_gap_capacity(tx: &Connection, kind: i64, limits: Limits) -> Result<(), ExportSpoolError> {
    // Adjacent gaps have already been removed; only the final row consumes a slot.
    let (total, source): (i64, i64) = tx
        .query_row(
            "SELECT count(*), COALESCE(sum(reason=0),0) FROM gaps",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| fail(&error))?;
    let total = usize::try_from(total).map_err(|_| corrupt_gap_count())?;
    let source = usize::try_from(source).map_err(|_| corrupt_gap_count())?;
    let permitted = if kind == 0 {
        limits.gap_slots
    } else {
        limits
            .gap_slots
            .saturating_sub(2_usize.saturating_sub(source))
    };
    if total >= permitted {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::SummaryFull,
            "exact export gap summary reserve is full",
        ));
    }
    Ok(())
}

fn corrupt_gap_count() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::IntegrityFailure,
        "invalid durable export gap count",
    )
}

pub(super) fn evict_oldest_telemetry(
    tx: &Connection,
    limits: Limits,
) -> Result<Option<u64>, ExportSpoolError> {
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT sequence, length(record_id)+length(committed_at)+length(payload)
             FROM pending WHERE durability=1 ORDER BY sequence LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| fail(&error))?;
    let Some((position, bytes)) = row else {
        return Ok(None);
    };
    let sequence = u64::try_from(position).map_err(|_| {
        ExportSpoolError::new(
            ExportSpoolErrorCode::IntegrityFailure,
            "invalid durable pending position",
        )
    })?;
    let bytes = nonnegative(bytes)?;
    insert_gap(
        tx,
        &ExportGap {
            durability: Durability::BestEffortTelemetry,
            first: sequence,
            last: sequence,
            reason: ExportGapReason::TelemetryEvicted,
        },
        limits,
    )?;
    tx.execute(
        "DELETE FROM pending WHERE durability=1 AND sequence=?1",
        [position],
    )
    .map_err(|error| fail(&error))?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn full_summary_merges_telemetry_without_losing_source_reserve() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE gaps (
                    durability INTEGER NOT NULL, first INTEGER NOT NULL,
                    last INTEGER NOT NULL, reason INTEGER NOT NULL,
                    PRIMARY KEY(durability, first)
                ) WITHOUT ROWID;",
            )
            .unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let limits = Limits::new(1024 * 1024, 3).unwrap();
        let gap = |durability, first, reason| ExportGap {
            durability,
            first,
            last: first,
            reason,
        };

        for first in [1, 3] {
            insert_gap(
                &tx,
                &gap(Durability::Critical, first, ExportGapReason::SourceExpired),
                limits,
            )
            .unwrap();
        }
        insert_gap(
            &tx,
            &gap(
                Durability::BestEffortTelemetry,
                1,
                ExportGapReason::TelemetryEvicted,
            ),
            limits,
        )
        .unwrap();
        insert_gap(
            &tx,
            &gap(
                Durability::BestEffortTelemetry,
                2,
                ExportGapReason::TelemetryEvicted,
            ),
            limits,
        )
        .unwrap();

        let (total, first, last): (i64, i64, i64) = tx
            .query_row(
                "SELECT count(*), min(first), max(last) FROM gaps WHERE durability=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((total, first, last), (1, 1, 2));
        let source_count: i64 = tx
            .query_row("SELECT count(*) FROM gaps WHERE reason=0", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(source_count, 2);

        let full = insert_gap(
            &tx,
            &gap(
                Durability::BestEffortTelemetry,
                4,
                ExportGapReason::TelemetryEvicted,
            ),
            limits,
        )
        .unwrap_err();
        assert_eq!(full.code(), ExportSpoolErrorCode::SummaryFull);
    }
}
