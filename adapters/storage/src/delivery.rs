use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use uob_application::{RecordedDeliveryAttempt, ScheduledDelivery, StorageError, StorageErrorCode};

use crate::{
    codec::{self, EncodedDelivery, EncodedDeliveryAttempt},
    configuration::unavailable,
};

pub(crate) fn read_pending<D: DeserializeOwned>(
    connection: &Connection,
    target: &str,
    revision: i64,
    ready_at: &str,
    limit: usize,
) -> Result<Vec<ScheduledDelivery<D>>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT d.target_instance_id, d.target_revision, d.event_id, d.delivery_id,\n\
                 d.ordering_key, d.deadline, d.durability, d.payload, d.attempt_count\n\
             FROM target_deliveries d\n\
             WHERE d.target_instance_id = ?1 AND d.target_revision = ?2\n\
               AND (d.next_attempt_at IS NULL OR d.next_attempt_at <= ?3)\n\
               AND NOT EXISTS (\n\
                   SELECT 1 FROM target_deliveries prior\n\
                   WHERE prior.target_instance_id = d.target_instance_id\n\
                     AND prior.target_revision = d.target_revision\n\
                     AND prior.ordering_key = d.ordering_key AND prior.rowid < d.rowid\n\
               )\n\
             ORDER BY d.rowid LIMIT ?4",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![target, revision, ready_at, limit_i64(limit)?],
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
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .map_err(unavailable)?;
    collect_rows(rows)?
        .into_iter()
        .map(|(a, b, c, d, e, f, g, h, attempts)| {
            let attempt_count = u32::try_from(attempts)
                .map_err(|_| integrity("delivery attempt count is outside u32 range"))?;
            let delivery = codec::decode_delivery(&EncodedDelivery {
                target_instance_id: a,
                target_revision: b,
                event_id: c,
                delivery_id: d,
                ordering_key: e,
                deadline: f,
                durability: g,
                payload: h,
            })?;
            Ok(ScheduledDelivery {
                delivery,
                attempt_count,
            })
        })
        .collect()
}

pub(crate) fn record_attempt(
    connection: &mut Connection,
    attempt: &EncodedDeliveryAttempt,
) -> Result<(), StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let exists = transaction
        .query_row(
            "SELECT 1 FROM target_deliveries WHERE delivery_id = ?1",
            [&attempt.delivery_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(unavailable)?
        .is_some();
    if !exists {
        return Err(StorageError::new(
            StorageErrorCode::Conflict,
            "delivery report does not match pending work",
        ));
    }
    transaction
        .execute(
            "INSERT INTO target_delivery_attempts(\n\
                 delivery_id, outcome, reported_at, resolution, retry_at\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                attempt.delivery_id,
                attempt.outcome,
                attempt.reported_at,
                attempt.resolution,
                attempt.retry_at
            ],
        )
        .map_err(unavailable)?;
    if attempt.resolution == 1 {
        transaction
            .execute(
                "DELETE FROM target_deliveries WHERE delivery_id = ?1",
                [&attempt.delivery_id],
            )
            .map_err(unavailable)?;
    } else {
        transaction
            .execute(
                "UPDATE target_deliveries SET attempt_count = attempt_count + 1,\n\
                     next_attempt_at = ?2 WHERE delivery_id = ?1",
                params![attempt.delivery_id, attempt.retry_at],
            )
            .map_err(unavailable)?;
    }
    transaction.commit().map_err(unavailable)
}

pub(crate) fn read_attempts(
    connection: &Connection,
    delivery_id: &str,
    limit: usize,
) -> Result<Vec<RecordedDeliveryAttempt>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT delivery_id, outcome, reported_at, resolution, retry_at\n\
             FROM target_delivery_attempts WHERE delivery_id = ?1 ORDER BY row_id DESC LIMIT ?2",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![delivery_id, limit_i64(limit)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(unavailable)?;
    collect_rows(rows)?
        .into_iter()
        .map(|(id, outcome, reported_at, resolution, retry_at)| {
            codec::decode_delivery_attempt(
                id,
                &outcome,
                &reported_at,
                resolution,
                retry_at.as_deref(),
            )
        })
        .collect()
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, StorageError> {
    rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
}

fn limit_i64(limit: usize) -> Result<i64, StorageError> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "delivery page limit exceeds SQLite integer range",
        )
    })
}

fn integrity(detail: &str) -> StorageError {
    StorageError::new(StorageErrorCode::IntegrityFailure, detail)
}
