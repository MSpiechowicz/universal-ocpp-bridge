use rusqlite::{Connection, params};
use uob_application::{
    CommittedRecordDescriptor, Durability, ExportGap, ExportGapReason, ExportSpoolError,
    ExportSpoolErrorCode, ExportSpoolGapCommit, ExportSpoolRecordBegin, ExportSpoolStatus,
};

use super::super::{
    pressure::{evict_oldest_telemetry, insert_gap, stream},
    store::nonnegative,
};
use super::{
    Limits, binding, blocked, complete, fail, invalid, prepare, rollback, sqlite_position, status,
};

pub(in crate::spool) struct ActiveRecord {
    pub progress: ExportSpoolGapCommit,
    pub row_id: i64,
    pub lengths: [u64; 3],
    pub offsets: [u64; 3],
}

pub(in crate::spool) enum BeginResult {
    Transfer(ActiveRecord),
    TelemetryDropped(ExportSpoolStatus),
}

pub(super) fn occupied(connection: &Connection, limits: Limits) -> Result<bool, ExportSpoolError> {
    let pages = nonnegative(
        connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .map_err(|error| fail(&error))?,
    )?;
    let free = nonnegative(
        connection
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .map_err(|error| fail(&error))?,
    )?;
    let reserve = (limits.main_bytes / 16).max(128 * 1024);
    if free > pages {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::IntegrityFailure,
            "invalid spool page count",
        ));
    }
    Ok((pages - free).saturating_add(reserve.div_ceil(4096)) <= limits.main_bytes / 4096)
}

struct Admission {
    lengths: [u64; 3],
    bytes: u64,
    capacity: u64,
    possible: bool,
}

fn admission(
    descriptor: &CommittedRecordDescriptor,
    limits: Limits,
) -> Result<Admission, ExportSpoolError> {
    let lengths = [
        descriptor.record_id_len,
        descriptor.committed_at_len,
        descriptor.payload_len,
    ];
    if lengths.contains(&0) {
        return Err(invalid());
    }
    let bytes = lengths
        .iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(*value))
        .ok_or_else(invalid)?;
    let reserve = (limits.main_bytes / 16).max(128 * 1024);
    let capacity = limits.main_bytes.saturating_sub(reserve + 64 * 1024);
    let possible = bytes <= capacity && lengths.iter().all(|value| i32::try_from(*value).is_ok());
    Ok(Admission {
        lengths,
        bytes,
        capacity,
        possible,
    })
}

fn check_critical_checkpoint(
    connection: &Connection,
    progress: &ExportSpoolGapCommit,
) -> Result<(), ExportSpoolError> {
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(&error))?;
    let checked = (|| {
        binding(connection, &progress.namespace)?;
        let existing = status(connection, &progress.namespace)?;
        if existing.critical != progress.expected {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::CheckpointConflict,
                "source checkpoint changed",
            ));
        }
        Ok(())
    })();
    rollback(connection);
    checked
}

fn attempt_begin(
    connection: &Connection,
    request: &ExportSpoolRecordBegin,
    limits: Limits,
    admission: &Admission,
    used: u64,
) -> Result<Option<i64>, ExportSpoolError> {
    let fits = admission.possible
        && used
            .checked_add(admission.bytes)
            .is_some_and(|total| total <= admission.capacity);
    if !fits || !occupied(connection, limits)? {
        return Ok(None);
    }

    connection
        .execute_batch("SAVEPOINT record_admission")
        .map_err(|error| fail(&error))?;
    let inserted = connection.execute(
        "INSERT INTO pending(durability,sequence,record_id,committed_at,payload) VALUES(?1,?2,zeroblob(?3),zeroblob(?4),zeroblob(?5))",
        params![stream(request.progress.durability),sqlite_position(request.descriptor.sequence)?,
            sqlite_position(admission.lengths[0])?,sqlite_position(admission.lengths[1])?,sqlite_position(admission.lengths[2])?],
    );
    let row_id = match inserted {
        Ok(_) => {
            if occupied(connection, limits)? {
                Some(connection.last_insert_rowid())
            } else {
                None
            }
        }
        Err(error) if fail(&error).code() == ExportSpoolErrorCode::Backpressure => None,
        Err(error) => return Err(fail(&error)),
    };
    if row_id.is_none() && connection.is_autocommit() {
        // SQLite may roll back the whole transaction on SQLITE_FULL.
        return Err(blocked());
    }
    if row_id.is_some() {
        connection
            .execute_batch("RELEASE record_admission")
            .map_err(|error| fail(&error))?;
    } else {
        connection
            .execute_batch("ROLLBACK TO record_admission; RELEASE record_admission")
            .map_err(|error| fail(&error))?;
    }
    Ok(row_id)
}

fn insert_telemetry_gap(
    connection: &Connection,
    progress: &ExportSpoolGapCommit,
    sequence: u64,
    limits: Limits,
) -> Result<(), ExportSpoolError> {
    insert_gap(
        connection,
        &ExportGap {
            durability: progress.durability,
            first: sequence,
            last: sequence,
            reason: ExportGapReason::TelemetryDropped,
        },
        limits,
    )?;
    complete(connection, progress)?;
    connection
        .execute_batch("COMMIT")
        .map_err(|error| fail(&error))
}

pub(in crate::spool) fn begin(
    connection: &Connection,
    request: &ExportSpoolRecordBegin,
    limits: Limits,
) -> Result<BeginResult, ExportSpoolError> {
    let progress = &request.progress;
    super::validate(progress, Some(&request.descriptor))?;
    let admission = admission(&request.descriptor, limits)?;
    if !admission.possible && progress.durability == Durability::Critical {
        check_critical_checkpoint(connection, progress)?;
        return Err(blocked());
    }

    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(&error))?;
    let result = (|| {
        prepare(connection, progress, limits)?;
        let used: i64 = connection
            .query_row(
                "SELECT COALESCE(sum(length(record_id)+length(committed_at)+length(payload)),0) FROM pending",
                [],
                |row| row.get(0),
            )
            .map_err(|error| fail(&error))?;
        let mut used = nonnegative(used)?;

        loop {
            if let Some(row_id) = attempt_begin(connection, request, limits, &admission, used)? {
                return Ok(BeginResult::Transfer(ActiveRecord {
                    progress: progress.clone(),
                    row_id,
                    lengths: admission.lengths,
                    offsets: [0; 3],
                }));
            }
            if progress.durability == Durability::BestEffortTelemetry {
                insert_telemetry_gap(connection, progress, request.descriptor.sequence, limits)?;
                return status(connection, &progress.namespace).map(BeginResult::TelemetryDropped);
            }

            let freed = match evict_oldest_telemetry(connection, limits) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return Err(blocked()),
                Err(error) if error.code() == ExportSpoolErrorCode::SummaryFull => {
                    return Err(blocked());
                }
                Err(error) => return Err(error),
            };
            used = used.checked_sub(freed).ok_or_else(|| {
                ExportSpoolError::new(
                    ExportSpoolErrorCode::IntegrityFailure,
                    "invalid durable pending byte count",
                )
            })?;
        }
    })();
    if result.is_err() {
        rollback(connection);
    }
    result
}
