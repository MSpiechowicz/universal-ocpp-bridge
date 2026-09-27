use rusqlite::{Connection, MAIN_DB, params};
use uob_application::{
    CommittedRecordDescriptor, CommittedRecordField, Durability, ExportGapReason,
    ExportSourceCheckpoint, ExportSpoolError, ExportSpoolErrorCode, ExportSpoolGapCommit,
    ExportSpoolNamespace, ExportSpoolStatus,
};

use super::{
    Limits, fail,
    pressure::{insert_gap, stream},
    store::{binding, status, validate_namespace},
};

mod begin;
use begin::occupied;
pub(super) use begin::{ActiveRecord, BeginResult, begin};

fn invalid() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::InvalidRequest,
        "invalid export source progress",
    )
}

fn blocked() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::Backpressure,
        "export record deferred at source checkpoint",
    )
}

pub(super) fn sqlite_position(position: u64) -> Result<i64, ExportSpoolError> {
    i64::try_from(position).map_err(|_| invalid())
}

pub(super) fn observe(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    durability: Durability,
    high_water: u64,
    legacy: bool,
) -> Result<ExportSpoolStatus, ExportSpoolError> {
    validate_namespace(ns)?;
    let revision = sqlite_position(ns.destination.configuration_revision)?;
    let high_water = sqlite_position(high_water)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(&error))?;
    let result = (|| {
        ensure_binding(connection, ns, revision)?;
        let column = match durability {
            Durability::Critical => "critical_high",
            Durability::BestEffortTelemetry => "telemetry_high",
        };
        connection.execute(
            &format!("UPDATE binding SET {column}=max({column},?1), incomplete=incomplete OR ?2, legacy=legacy OR ?2 WHERE id=1"),
            params![high_water, legacy],
        ).map_err(|error| fail(&error))?;
        connection
            .execute_batch("COMMIT")
            .map_err(|error| fail(&error))?;
        Ok(())
    })();
    if result.is_err() {
        rollback(connection);
    }
    result?;
    status(connection, ns)
}

fn ensure_binding(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    revision: i64,
) -> Result<(), ExportSpoolError> {
    if !binding(connection, ns)? {
        connection.execute(
            "INSERT INTO binding(id,destination,revision,kind,generation) VALUES(1,?1,?2,?3,?4)",
            params![ns.destination.destination_id.as_str(), revision, ns.provider_kind, ns.source_generation],
        ).map_err(|error| fail(&error))?;
    }
    Ok(())
}

fn check_cursor(
    ns: &ExportSpoolNamespace,
    durability: Durability,
    checkpoint: &ExportSourceCheckpoint,
) -> Result<(), ExportSpoolError> {
    let prefix = format!(
        "uob:record:v1:{}:{}:",
        ns.source_generation,
        stream(durability)
    );
    if checkpoint
        .cursor
        .as_str()
        .strip_prefix(&prefix)
        .and_then(|number| number.parse::<u64>().ok())
        != Some(checkpoint.sequence)
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate(
    progress: &ExportSpoolGapCommit,
    record: Option<&CommittedRecordDescriptor>,
) -> Result<(), ExportSpoolError> {
    validate_namespace(&progress.namespace)?;
    sqlite_position(progress.high_water)?;
    sqlite_position(progress.next.sequence)?;
    let previous = progress.expected.as_ref().map_or(0, |point| point.sequence);
    if let Some(expected) = &progress.expected {
        check_cursor(&progress.namespace, progress.durability, expected)?;
    }
    check_cursor(&progress.namespace, progress.durability, &progress.next)?;
    if progress.next.sequence > progress.high_water
        || progress.next.sequence < previous
        || progress.gaps.len() > 101
    {
        return Err(invalid());
    }
    let mut covered = previous;
    for gap in &progress.gaps {
        if gap.durability != progress.durability
            || gap.reason != ExportGapReason::SourceExpired
            || gap.first != covered.checked_add(1).ok_or_else(invalid)?
            || gap.last < gap.first
        {
            return Err(invalid());
        }
        covered = gap.last;
    }
    if let Some(descriptor) = record {
        if descriptor.durability != progress.durability
            || descriptor.sequence != covered.checked_add(1).ok_or_else(invalid)?
            || descriptor.cursor != progress.next.cursor
            || descriptor.token.generation() != progress.namespace.source_generation
            || descriptor.token.sequence() != descriptor.sequence
            || descriptor.token.durability() != descriptor.durability
            || descriptor.token.lengths()
                != [
                    descriptor.record_id_len,
                    descriptor.committed_at_len,
                    descriptor.payload_len,
                ]
        {
            return Err(invalid());
        }
        covered = descriptor.sequence;
    }
    if covered != progress.next.sequence {
        return Err(invalid());
    }
    Ok(())
}

fn prepare(
    connection: &Connection,
    progress: &ExportSpoolGapCommit,
    limits: Limits,
) -> Result<(), ExportSpoolError> {
    ensure_binding(
        connection,
        &progress.namespace,
        sqlite_position(progress.namespace.destination.configuration_revision)?,
    )?;
    let existing = status(connection, &progress.namespace)?;
    let checkpoint = match progress.durability {
        Durability::Critical => existing.critical,
        Durability::BestEffortTelemetry => existing.telemetry,
    };
    if checkpoint != progress.expected {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::CheckpointConflict,
            "source checkpoint changed",
        ));
    }
    for gap in &progress.gaps {
        insert_gap(connection, gap, limits)?;
    }
    Ok(())
}

fn complete(
    connection: &Connection,
    progress: &ExportSpoolGapCommit,
) -> Result<(), ExportSpoolError> {
    let (cursor, position, high) = match progress.durability {
        Durability::Critical => ("critical_cursor", "critical_seq", "critical_high"),
        Durability::BestEffortTelemetry => ("telemetry_cursor", "telemetry_seq", "telemetry_high"),
    };
    let has_gaps: bool = connection
        .query_row("SELECT EXISTS(SELECT 1 FROM gaps)", [], |row| row.get(0))
        .map_err(|error| fail(&error))?;
    connection.execute(&format!(
        "UPDATE binding SET {cursor}=?1,{position}=?2,{high}=max({high},?3),incomplete=incomplete OR ?4 OR ?5,legacy=legacy OR ?5 WHERE id=1"
    ), params![progress.next.cursor.as_str(),sqlite_position(progress.next.sequence)?,
        sqlite_position(progress.high_water)?,has_gaps,progress.legacy_baseline_incomplete])
        .map_err(|error| fail(&error))?;
    Ok(())
}

pub(super) fn rollback(connection: &Connection) {
    if !connection.is_autocommit() {
        let _ = connection.execute_batch("ROLLBACK");
    }
}

pub(super) fn commit_gaps(
    connection: &Connection,
    progress: &ExportSpoolGapCommit,
    limits: Limits,
) -> Result<ExportSpoolStatus, ExportSpoolError> {
    validate(progress, None)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(&error))?;
    let result = (|| {
        prepare(connection, progress, limits)?;
        complete(connection, progress)?;
        connection
            .execute_batch("COMMIT")
            .map_err(|error| fail(&error))?;
        Ok(())
    })();
    if result.is_err() {
        rollback(connection);
    }
    result?;
    status(connection, &progress.namespace)
}

pub(super) fn append(
    connection: &Connection,
    active: &mut ActiveRecord,
    chunk: &uob_application::BudgetedRecordChunk,
) -> Result<(), ExportSpoolError> {
    let index = chunk.field.index();
    if index > 0 && active.offsets[..index] != active.lengths[..index]
        || chunk.offset != active.offsets[index]
        || chunk.bytes.is_empty()
        || !chunk.is_valid_for(active.lengths[index])
        || active.offsets[index + 1..]
            .iter()
            .any(|offset| *offset != 0)
    {
        return Err(invalid());
    }
    let column = match chunk.field {
        CommittedRecordField::RecordId => "record_id",
        CommittedRecordField::CommittedAt => "committed_at",
        CommittedRecordField::Payload => "payload",
    };
    let mut blob = connection
        .blob_open(MAIN_DB, "pending", column, active.row_id, false)
        .map_err(|error| fail(&error))?;
    blob.write_at(
        &chunk.bytes,
        usize::try_from(chunk.offset).map_err(|_| invalid())?,
    )
    .map_err(|error| fail(&error))?;
    active.offsets[index] = chunk.next_offset;
    Ok(())
}

pub(super) fn finish(
    connection: &Connection,
    active: &ActiveRecord,
    limits: Limits,
) -> Result<ExportSpoolStatus, ExportSpoolError> {
    if active.lengths != active.offsets {
        return Err(invalid());
    }
    if !occupied(connection, limits)? {
        return Err(blocked());
    }
    complete(connection, &active.progress)?;
    connection
        .execute_batch("COMMIT")
        .map_err(|error| fail(&error))?;
    status(connection, &active.progress.namespace)
}
