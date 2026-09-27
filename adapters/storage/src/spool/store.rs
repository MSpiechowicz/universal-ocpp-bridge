use rusqlite::{Connection, MAIN_DB, OptionalExtension, params};
use uob_application::{
    BudgetedRecordChunk, CommittedRecordCursor, CommittedRecordField, Durability,
    EXPORT_RECORD_CHUNK_BYTES, ExportGap, ExportPendingDescriptor, ExportPendingPage,
    ExportPendingPosition, ExportSourceCheckpoint, ExportSpoolError, ExportSpoolErrorCode,
    ExportSpoolNamespace, ExportSpoolStatus, PageLimit, RuntimeResourceBudget, WorkClass,
};

use super::{
    fail,
    pressure::{decode_reason, stream},
};

fn conflict() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::NamespaceConflict,
        "pending export data belongs to a different destination or source",
    )
}

pub(super) fn validate_namespace(ns: &ExportSpoolNamespace) -> Result<(), ExportSpoolError> {
    if ns.provider_kind.trim().is_empty()
        || ns.provider_kind.len() > 128
        || ns.source_generation.trim().is_empty()
        || ns.source_generation.len() > 128
        || ns.source_generation.contains(':')
        || ns.destination.destination_id.as_str().len() > 256
        || ns.destination.configuration_revision > i64::MAX as u64
    {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "invalid or oversized export namespace",
        ));
    }
    Ok(())
}

pub(super) fn binding(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
) -> Result<bool, ExportSpoolError> {
    let stored: Option<(String, i64, String, String)> = connection
        .query_row(
            "SELECT destination, revision, kind, generation FROM binding WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| fail(&error))?;
    let Some((id, revision, kind, generation)) = stored else {
        return Ok(false);
    };
    if id != ns.destination.destination_id.as_str()
        || u64::try_from(revision).ok() != Some(ns.destination.configuration_revision)
        || kind != ns.provider_kind
        || generation != ns.source_generation
    {
        return Err(conflict());
    }
    Ok(true)
}

pub(super) fn status(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
) -> Result<ExportSpoolStatus, ExportSpoolError> {
    validate_namespace(ns)?;
    let existing = binding(connection, ns)?;
    let mut status = ExportSpoolStatus {
        destination: ns.destination.clone(),
        provider_kind: ns.provider_kind.clone(),
        source_generation: ns.source_generation.clone(),
        critical: None,
        telemetry: None,
        critical_high_water: 0,
        telemetry_high_water: 0,
        pending_records: 0,
        gaps: Vec::new(),
        incomplete: false,
        legacy_baseline_incomplete: false,
    };
    if !existing {
        return Ok(status);
    }
    let values: (
        Option<String>,
        i64,
        Option<String>,
        i64,
        i64,
        i64,
        bool,
        bool,
    ) = connection
        .query_row(
            "SELECT critical_cursor, critical_seq, telemetry_cursor, telemetry_seq,
            critical_high, telemetry_high, incomplete, legacy FROM binding WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .map_err(|error| fail(&error))?;
    status.critical = checkpoint(values.0, values.1)?;
    status.telemetry = checkpoint(values.2, values.3)?;
    status.critical_high_water = nonnegative(values.4)?;
    status.telemetry_high_water = nonnegative(values.5)?;
    status.incomplete = values.6;
    status.legacy_baseline_incomplete = values.7;
    status.pending_records = nonnegative(
        connection
            .query_row("SELECT count(*) FROM pending", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| fail(&error))?,
    )?;
    let mut statement = connection
        .prepare(
            "SELECT durability, first, last, reason FROM gaps ORDER BY durability, first LIMIT 513",
        )
        .map_err(|error| fail(&error))?;
    let mut rows = statement.query([]).map_err(|error| fail(&error))?;
    while let Some(row) = rows.next().map_err(|error| fail(&error))? {
        if status.gaps.len() == super::MAX_GAPS {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::IntegrityFailure,
                "durable export summaries exceed their bounded capacity",
            ));
        }
        let d: i64 = row.get(0).map_err(|error| fail(&error))?;
        let first = nonnegative(row.get(1).map_err(|error| fail(&error))?)?;
        let last = nonnegative(row.get(2).map_err(|error| fail(&error))?)?;
        if first == 0 || last < first {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::IntegrityFailure,
                "durable export gap interval is invalid",
            ));
        }
        status.gaps.push(ExportGap {
            durability: match d {
                0 => Durability::Critical,
                1 => Durability::BestEffortTelemetry,
                _ => {
                    return Err(ExportSpoolError::new(
                        ExportSpoolErrorCode::IntegrityFailure,
                        "invalid durable export stream",
                    ));
                }
            },
            first,
            last,
            reason: decode_reason(row.get(3).map_err(|error| fail(&error))?)?,
        });
    }
    Ok(status)
}

pub(super) fn nonnegative(value: i64) -> Result<u64, ExportSpoolError> {
    u64::try_from(value).map_err(|_| {
        ExportSpoolError::new(
            ExportSpoolErrorCode::IntegrityFailure,
            "invalid durable source position",
        )
    })
}

fn checkpoint(
    cursor: Option<String>,
    sequence: i64,
) -> Result<Option<ExportSourceCheckpoint>, ExportSpoolError> {
    cursor
        .map(|cursor| {
            Ok(ExportSourceCheckpoint {
                cursor: CommittedRecordCursor::new(cursor).map_err(|_| {
                    ExportSpoolError::new(
                        ExportSpoolErrorCode::IntegrityFailure,
                        "invalid durable source checkpoint",
                    )
                })?,
                sequence: nonnegative(sequence)?,
            })
        })
        .transpose()
}

pub(super) fn pending(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    after: Option<ExportPendingPosition>,
    limit: PageLimit,
    budget: &RuntimeResourceBudget,
) -> Result<ExportPendingPage, ExportSpoolError> {
    binding(connection, ns)?;
    let reservation = budget
        .try_reserve(WorkClass::ExporterBatch, usize::from(limit.get()) * 128)
        .map_err(|_| {
            ExportSpoolError::new(
                ExportSpoolErrorCode::Backpressure,
                "pending metadata memory unavailable",
            )
        })?;
    let (durability, sequence) = match after {
        Some(point) => (
            stream(point.durability),
            i64::try_from(point.sequence).map_err(|_| {
                ExportSpoolError::new(
                    ExportSpoolErrorCode::InvalidRequest,
                    "invalid pending continuation",
                )
            })?,
        ),
        None => (-1_i64, -1_i64),
    };
    let mut statement = connection.prepare(
        "SELECT row_id,durability,sequence,length(record_id),length(committed_at),length(payload)
         FROM pending WHERE durability > ?1 OR (durability = ?1 AND sequence > ?2)
         ORDER BY durability,sequence LIMIT ?3",
    ).map_err(|error| fail(&error))?;
    let mut rows = statement
        .query(params![durability, sequence, i64::from(limit.get()) + 1])
        .map_err(|error| fail(&error))?;
    let mut items = Vec::with_capacity(usize::from(limit.get()));
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(|error| fail(&error))? {
        if items.len() == usize::from(limit.get()) {
            has_more = true;
            break;
        }
        let durability = match row.get::<_, i64>(1).map_err(|error| fail(&error))? {
            0 => Durability::Critical,
            1 => Durability::BestEffortTelemetry,
            _ => return Err(invalid_pending()),
        };
        let length = |index| -> Result<u64, ExportSpoolError> {
            nonnegative(row.get::<_, i64>(index).map_err(|error| fail(&error))?)
        };
        items.push(ExportPendingDescriptor {
            row_id: row.get(0).map_err(|error| fail(&error))?,
            position: ExportPendingPosition {
                durability,
                sequence: length(2)?,
            },
            record_id_len: length(3)?,
            committed_at_len: length(4)?,
            payload_len: length(5)?,
            encoding_version: 1,
        });
    }
    let resume = items.last().map(|item| item.position);
    Ok(ExportPendingPage {
        items,
        resume,
        has_more,
        reservation: Some(reservation),
    })
}

fn invalid_pending() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::IntegrityFailure,
        "invalid durable pending record",
    )
}

pub(super) fn pending_chunk(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    descriptor: &ExportPendingDescriptor,
    field: CommittedRecordField,
    offset: u64,
    max_bytes: usize,
    budget: &RuntimeResourceBudget,
) -> Result<BudgetedRecordChunk, ExportSpoolError> {
    binding(connection, ns)?;
    if max_bytes == 0
        || max_bytes > EXPORT_RECORD_CHUNK_BYTES
        || descriptor.row_id <= 0
        || descriptor.position.sequence == 0
        || descriptor.encoding_version != 1
    {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "invalid pending range",
        ));
    }
    let saved: Option<(i64, i64, i64, i64, i64)> = connection
        .query_row(
            "SELECT durability,sequence,length(record_id),length(committed_at),length(payload)
         FROM pending WHERE row_id=?1",
            [descriptor.row_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| fail(&error))?;
    let Some((durability, sequence, id_len, at_len, payload_len)) = saved else {
        if descriptor.position.durability == Durability::Critical {
            return Err(invalid_pending());
        }
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::PendingExpired,
            "pending telemetry was evicted",
        ));
    };
    if durability != stream(descriptor.position.durability)
        || i64::try_from(descriptor.position.sequence).ok() != Some(sequence)
        || [id_len, at_len, payload_len]
            != [
                i64::try_from(descriptor.record_id_len).unwrap_or(-1),
                i64::try_from(descriptor.committed_at_len).unwrap_or(-1),
                i64::try_from(descriptor.payload_len).unwrap_or(-1),
            ]
    {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "pending descriptor identity changed",
        ));
    }
    let (column, field_len) = match field {
        CommittedRecordField::RecordId => ("record_id", descriptor.record_id_len),
        CommittedRecordField::CommittedAt => ("committed_at", descriptor.committed_at_len),
        CommittedRecordField::Payload => ("payload", descriptor.payload_len),
    };
    if offset > field_len || offset.checked_add(max_bytes as u64).is_none() {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "invalid pending range",
        ));
    }
    let count = usize::try_from((field_len - offset).min(max_bytes as u64))
        .map_err(|_| invalid_pending())?;
    let guard = budget
        .try_reserve(WorkClass::ExporterBatch, count)
        .map_err(|_| {
            ExportSpoolError::new(
                ExportSpoolErrorCode::Backpressure,
                "pending read memory unavailable",
            )
        })?;
    let mut bytes = vec![0; count];
    if count > 0 {
        let blob = connection
            .blob_open(MAIN_DB, "pending", column, descriptor.row_id, true)
            .map_err(|error| fail(&error))?;
        if u64::try_from(blob.len()).map_err(|_| invalid_pending())? != field_len {
            return Err(invalid_pending());
        }
        blob.read_at_exact(
            &mut bytes,
            usize::try_from(offset).map_err(|_| invalid_pending())?,
        )
        .map_err(|error| fail(&error))?;
    }
    BudgetedRecordChunk::new(field, offset, bytes, field_len, guard).map_err(|_| invalid_pending())
}
