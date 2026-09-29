use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};
use uob_application::{
    Durability, ExportDeliveryClaim, ExportPendingDescriptor, ExportPendingPosition,
    ExportSpoolError, ExportSpoolErrorCode, ExportSpoolNamespace, ExportSpoolStatus, PageLimit,
    RuntimeResourceBudget, WorkClass,
};
use uob_contracts::{ExportBatchId, ExportOutcome, ExportRecordId, ExportReport};

use super::{fail, store, store_commit};

const MAX_RECORDS: usize = 100;
const MAX_BATCH_BYTES: usize = 256 * 1024;
// Worst-case JSON escapes one byte as six ASCII bytes. The additional per-record
// envelope reserve is deliberately conservative; the sender also checks the exact batch.
const RECORD_OVERHEAD: usize = 4096;

fn conflict() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::CheckpointConflict,
        "delivery claim changed",
    )
}

fn invalid() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::InvalidRequest,
        "invalid delivery batch limit",
    )
}

fn integrity() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::IntegrityFailure,
        "invalid durable delivery claim",
    )
}

fn oversized() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::DeliveryLimitExceeded,
        "pending record or pinned claim exceeds delivery batch limit",
    )
}

fn memory_pressure() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::Backpressure,
        "delivery claim memory unavailable",
    )
}

fn reserve(
    budget: &RuntimeResourceBudget,
    count: usize,
) -> Result<Option<uob_application::RuntimeReservation>, ExportSpoolError> {
    budget
        .try_reserve(WorkClass::ExporterBatch, count * 128)
        .map(Some)
        .map_err(|_| memory_pressure())
}

fn descriptor(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportPendingDescriptor> {
    let durability = match row.get::<_, i64>(1)? {
        0 => Durability::Critical,
        1 => Durability::BestEffortTelemetry,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let length = |column| -> rusqlite::Result<u64> {
        let value: i64 = row.get(column)?;
        u64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery)
    };
    Ok(ExportPendingDescriptor {
        row_id: row.get(0)?,
        position: ExportPendingPosition {
            durability,
            sequence: length(2)?,
        },
        record_id_len: length(3)?,
        committed_at_len: length(4)?,
        payload_len: length(5)?,
        encoding_version: 1,
    })
}

fn existing(
    connection: &Connection,
) -> Result<Option<(ExportBatchId, Vec<ExportPendingDescriptor>)>, ExportSpoolError> {
    let id: Option<String> = connection
        .query_row("SELECT batch_id FROM delivery WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| fail(&error))?;
    let Some(id) = id else { return Ok(None) };
    let id = ExportBatchId::new(id).map_err(|_| integrity())?;
    let mut statement = connection
        .prepare(
            "SELECT p.row_id,p.durability,p.sequence,length(p.record_id),
                length(p.committed_at),length(p.payload)
         FROM delivery_items AS d JOIN pending AS p ON p.row_id=d.row_id
         ORDER BY d.ordinal LIMIT 101",
        )
        .map_err(|error| fail(&error))?;
    let items = statement
        .query_map([], descriptor)
        .map_err(|error| fail(&error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| fail(&error))?;
    if items.is_empty() || items.len() > MAX_RECORDS {
        return Err(integrity());
    }
    Ok(Some((id, items)))
}

fn record_id(
    connection: &Connection,
    item: &ExportPendingDescriptor,
) -> Result<ExportRecordId, ExportSpoolError> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_id FROM pending WHERE row_id=?1",
            [item.row_id],
            |row| row.get(0),
        )
        .map_err(|error| fail(&error))?;
    if bytes.len() as u64 != item.record_id_len {
        return Err(integrity());
    }
    ExportRecordId::new(String::from_utf8(bytes).map_err(|_| integrity())?).map_err(|_| integrity())
}

pub(super) fn claim(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    limit: PageLimit,
    max_bytes: usize,
    budget: &RuntimeResourceBudget,
) -> Result<Option<ExportDeliveryClaim>, ExportSpoolError> {
    store::validate_namespace(ns)?;
    if limit.get() == 0
        || usize::from(limit.get()) > MAX_RECORDS
        || !(1..=MAX_BATCH_BYTES).contains(&max_bytes)
    {
        return Err(invalid());
    }

    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(&error))?;
    let result = (|| {
        let claim = claim_in_transaction(connection, ns, limit, max_bytes, budget)?;
        connection
            .execute_batch("COMMIT")
            .map_err(|error| fail(&error))?;
        Ok(claim)
    })();
    if result.is_err() {
        store_commit::rollback(connection);
    }
    result
}

fn claim_in_transaction(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    limit: PageLimit,
    max_bytes: usize,
    budget: &RuntimeResourceBudget,
) -> Result<Option<ExportDeliveryClaim>, ExportSpoolError> {
    let bound = store::binding(connection, ns)?;
    if let Some((batch_id, items)) = existing(connection)? {
        return replay_claim(batch_id, items, limit, max_bytes, budget).map(Some);
    }
    if !bound {
        return Ok(None);
    }

    let page = store::pending(connection, ns, None, limit, budget)?;
    let selected = select_items(connection, page.items, max_bytes)?;
    if selected.is_empty() {
        return Ok(None);
    }

    let batch_id = persist_claim(connection, ns, &selected)?;
    let reservation = reserve(budget, selected.len())?;
    Ok(Some(ExportDeliveryClaim {
        batch_id,
        items: selected,
        reservation,
    }))
}

fn replay_claim(
    batch_id: ExportBatchId,
    items: Vec<ExportPendingDescriptor>,
    limit: PageLimit,
    max_bytes: usize,
    budget: &RuntimeResourceBudget,
) -> Result<ExportDeliveryClaim, ExportSpoolError> {
    if items.len() > usize::from(limit.get()) {
        return Err(oversized());
    }
    let estimated = items
        .iter()
        .try_fold(256_usize, |size, item| {
            let fields = item
                .record_id_len
                .checked_add(item.committed_at_len)?
                .checked_add(item.payload_len)?;
            let fields = usize::try_from(fields).ok()?;
            size.checked_add(fields.checked_mul(6)?.checked_add(RECORD_OVERHEAD)?)
        })
        .ok_or_else(oversized)?;
    if estimated > max_bytes {
        return Err(oversized());
    }

    let reservation = reserve(budget, items.len())?;
    Ok(ExportDeliveryClaim {
        batch_id,
        items,
        reservation,
    })
}

fn select_items(
    connection: &Connection,
    items: Vec<ExportPendingDescriptor>,
    max_bytes: usize,
) -> Result<Vec<ExportPendingDescriptor>, ExportSpoolError> {
    let mut selected = Vec::new();
    let mut estimated = 256_usize;
    let mut identities = BTreeSet::new();
    for item in items {
        let fields = item
            .record_id_len
            .checked_add(item.committed_at_len)
            .and_then(|bytes| bytes.checked_add(item.payload_len))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(oversized)?;
        let next = fields
            .checked_mul(6)
            .and_then(|bytes| bytes.checked_add(RECORD_OVERHEAD))
            .and_then(|bytes| estimated.checked_add(bytes))
            .ok_or_else(oversized)?;
        if next > max_bytes {
            if selected.is_empty() {
                return Err(oversized());
            }
            break;
        }
        let id = record_id(connection, &item)?;
        if !identities.insert(id) {
            return Err(integrity());
        }
        estimated = next;
        selected.push(item);
    }
    Ok(selected)
}

fn persist_claim(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    selected: &[ExportPendingDescriptor],
) -> Result<ExportBatchId, ExportSpoolError> {
    let sequence: i64 = connection
        .query_row(
            "SELECT next_delivery_id FROM binding WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| fail(&error))?;
    let next = sequence
        .checked_add(1)
        .filter(|value| *value > 0)
        .ok_or_else(integrity)?;
    let batch_id = ExportBatchId::new(format!(
        "uob:delivery:v1:{}:{}:{}:{next}",
        ns.destination.destination_id.as_str(),
        ns.destination.configuration_revision,
        ns.source_generation
    ))
    .map_err(|_| integrity())?;
    connection
        .execute("UPDATE binding SET next_delivery_id=?1 WHERE id=1", [next])
        .map_err(|error| fail(&error))?;
    connection
        .execute(
            "INSERT INTO delivery(id,batch_id) VALUES(1,?1)",
            [batch_id.as_str()],
        )
        .map_err(|error| fail(&error))?;
    for (ordinal, item) in selected.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO delivery_items(ordinal,row_id) VALUES(?1,?2)",
                params![
                    i64::try_from(ordinal).map_err(|_| integrity())?,
                    item.row_id
                ],
            )
            .map_err(|error| fail(&error))?;
    }
    Ok(batch_id)
}

pub(super) fn settle(
    connection: &Connection,
    ns: &ExportSpoolNamespace,
    report: &ExportReport,
) -> Result<ExportSpoolStatus, ExportSpoolError> {
    store::validate_namespace(ns)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(&error))?;
    let result = (|| {
        store::binding(connection, ns)?;
        let (batch_id, items) = existing(connection)?.ok_or_else(conflict)?;
        if report.destination() != &ns.destination
            || report.batch_id() != &batch_id
            || report.record_ids().len() != items.len()
        {
            return Err(conflict());
        }
        for (identity, item) in report.record_ids().iter().zip(&items) {
            if identity.subrecord_id.is_some() || identity.record_id != record_id(connection, item)?
            {
                return Err(conflict());
            }
        }
        if matches!(report.outcome(), ExportOutcome::Partial { .. }) {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::InvalidRequest,
                "partial remote reports require independent reconciliation",
            ));
        }
        if !matches!(report.outcome(), ExportOutcome::Committed) {
            // No local progress follows retryable, permanent, or uncertain results.
            // The same exact claim survives restart and is retried with its original batch ID.
            connection
                .execute_batch("COMMIT")
                .map_err(|error| fail(&error))?;
            return store::status(connection, ns);
        }
        connection
            .execute("DELETE FROM delivery_items", [])
            .map_err(|error| fail(&error))?;
        for item in &items {
            if connection
                .execute("DELETE FROM pending WHERE row_id=?1", [item.row_id])
                .map_err(|error| fail(&error))?
                != 1
            {
                return Err(integrity());
            }
        }
        connection
            .execute("DELETE FROM delivery WHERE id=1", [])
            .map_err(|error| fail(&error))?;
        let changed = connection
            .execute(
                "UPDATE binding SET last_confirmed_batch=?1,
                confirmed_records=confirmed_records+?2 WHERE id=1",
                params![
                    batch_id.as_str(),
                    i64::try_from(items.len()).map_err(|_| integrity())?
                ],
            )
            .map_err(|error| fail(&error))?;
        if changed != 1 {
            return Err(integrity());
        }
        connection
            .execute_batch("COMMIT")
            .map_err(|error| fail(&error))?;
        store::status(connection, ns)
    })();
    if result.is_err() {
        store_commit::rollback(connection);
    }
    result
}
