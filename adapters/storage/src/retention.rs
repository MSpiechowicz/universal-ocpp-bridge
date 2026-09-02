use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use uob_application::{
    DEFAULT_ACTIVE_SESSION_RESERVE_BYTES, DEFAULT_OPERATIONAL_STORAGE_BUDGET_BYTES,
    StorageAdmissionState, StorageError, StorageErrorCode, StorageRetentionStatus,
    StorageWritePurpose,
};

use crate::{
    codec::{EncodedDelivery, EncodedEvent, EncodedRecord, EncodedWrite},
    configuration::unavailable,
};

const DROPPED_TELEMETRY: &str = "dropped_best_effort_telemetry";
const DROPPED_DELIVERIES: &str = "dropped_best_effort_deliveries";
const PRUNED_EVENTS: &str = "pruned_expired_events";
const PRUNED_ATTEMPTS: &str = "pruned_expired_delivery_attempts";

/// Logical storage limits used by the authoritative SQLite journal and outbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SqliteRetentionPolicy {
    /// Maximum retained logical bytes.
    pub budget_bytes: u64,
    /// Portion unavailable to new-session starts.
    pub active_session_reserve_bytes: u64,
}

impl SqliteRetentionPolicy {
    /// Validates a non-zero budget with a strictly smaller completion reserve.
    ///
    /// # Errors
    ///
    /// Returns [`StorageErrorCode::InvalidRequest`] when the budget is zero or does not exceed
    /// the reserve.
    pub fn new(budget_bytes: u64, active_session_reserve_bytes: u64) -> Result<Self, StorageError> {
        if budget_bytes == 0 || active_session_reserve_bytes >= budget_bytes {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "storage budget must exceed its active-session reserve",
            ));
        }
        Ok(Self {
            budget_bytes,
            active_session_reserve_bytes,
        })
    }

    const fn start_limit(self) -> u64 {
        self.budget_bytes - self.active_session_reserve_bytes
    }
}

impl Default for SqliteRetentionPolicy {
    fn default() -> Self {
        Self {
            budget_bytes: DEFAULT_OPERATIONAL_STORAGE_BUDGET_BYTES,
            active_session_reserve_bytes: DEFAULT_ACTIVE_SESSION_RESERVE_BYTES,
        }
    }
}

pub(crate) fn prepare_write(
    transaction: &Transaction<'_>,
    policy: SqliteRetentionPolicy,
    write: &mut EncodedWrite,
) -> Result<(), StorageError> {
    let limit = match write.purpose {
        StorageWritePurpose::Routine | StorageWritePurpose::NewSessionStart => policy.start_limit(),
        StorageWritePurpose::ActiveSessionCompletion => policy.budget_bytes,
    };
    let critical = critical_write_bytes(write)?;
    let mut used = used_bytes(transaction)?;
    let must_fit = critical > 0 || write.purpose == StorageWritePurpose::NewSessionStart;
    if must_fit && used.saturating_add(critical) > limit {
        shed_existing_best_effort(transaction)?;
        used = used_bytes(transaction)?;
    }
    if must_fit && used.saturating_add(critical) > limit {
        return Err(StorageError::new(
            StorageErrorCode::CapacityExhausted,
            "critical operational storage capacity cannot be maintained",
        ));
    }

    let mut remaining = limit.saturating_sub(used.saturating_add(critical));
    let dropped_deliveries = retain_fitting_deliveries(&mut write.deliveries, &mut remaining);
    let dropped_records = retain_fitting_records(&mut write.records, &mut remaining);
    increment_stat(transaction, DROPPED_DELIVERIES, dropped_deliveries)?;
    increment_stat(transaction, DROPPED_TELEMETRY, dropped_records)?;
    Ok(())
}

pub(crate) fn maintain(
    connection: &mut Connection,
    policy: SqliteRetentionPolicy,
    now: i64,
) -> Result<StorageRetentionStatus, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let pruned_events = transaction
        .execute(
            "DELETE FROM journal_events AS event
             WHERE event.retain_until IS NOT NULL AND event.retain_until <= ?1
               AND NOT EXISTS (
                   SELECT 1 FROM target_deliveries AS delivery
                   WHERE delivery.event_id = event.event_id
               )",
            [now],
        )
        .map_err(unavailable)?;
    let pruned_attempts = transaction
        .execute(
            "DELETE FROM target_delivery_attempts AS attempt
             WHERE attempt.retain_until IS NOT NULL AND attempt.retain_until <= ?1
               AND NOT EXISTS (
                   SELECT 1 FROM target_deliveries AS delivery
                   WHERE delivery.delivery_id = attempt.delivery_id
               )",
            [now],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "DELETE FROM committed_records
             WHERE durability = 1 AND retain_until IS NOT NULL AND retain_until <= ?1",
            [now],
        )
        .map_err(unavailable)?;
    increment_stat(&transaction, PRUNED_EVENTS, count(pruned_events)?)?;
    increment_stat(&transaction, PRUNED_ATTEMPTS, count(pruned_attempts)?)?;
    transaction.commit().map_err(unavailable)?;
    status(connection, policy)
}

pub(crate) fn status(
    connection: &Connection,
    policy: SqliteRetentionPolicy,
) -> Result<StorageRetentionStatus, StorageError> {
    let used_bytes = used_bytes(connection)?;
    Ok(StorageRetentionStatus {
        budget_bytes: policy.budget_bytes,
        active_session_reserve_bytes: policy.active_session_reserve_bytes,
        used_bytes,
        new_session_admission: if used_bytes >= policy.start_limit() {
            StorageAdmissionState::CriticalCapacityExhausted
        } else {
            StorageAdmissionState::Available
        },
        retained_critical_events: scalar(connection, "SELECT COUNT(*) FROM journal_events")?,
        retained_required_deliveries: scalar(
            connection,
            "SELECT COUNT(*) FROM target_deliveries WHERE durability = 0",
        )?,
        dropped_best_effort_telemetry: stat(connection, DROPPED_TELEMETRY)?,
        dropped_best_effort_deliveries: stat(connection, DROPPED_DELIVERIES)?,
        pruned_expired_events: stat(connection, PRUNED_EVENTS)?,
        pruned_expired_delivery_attempts: stat(connection, PRUNED_ATTEMPTS)?,
    })
}

fn shed_existing_best_effort(transaction: &Transaction<'_>) -> Result<(), StorageError> {
    let telemetry = transaction
        .execute("DELETE FROM committed_records WHERE durability = 1", [])
        .map_err(unavailable)?;
    let deliveries = transaction
        .execute("DELETE FROM target_deliveries WHERE durability = 1", [])
        .map_err(unavailable)?;
    increment_stat(transaction, DROPPED_TELEMETRY, count(telemetry)?)?;
    increment_stat(transaction, DROPPED_DELIVERIES, count(deliveries)?)
}

fn retain_fitting_deliveries(values: &mut Vec<EncodedDelivery>, remaining: &mut u64) -> u64 {
    let mut dropped = 0_u64;
    values.retain(|value| {
        if value.durability == 0 {
            return true;
        }
        let bytes = delivery_bytes(value).unwrap_or(u64::MAX);
        if bytes <= *remaining {
            *remaining -= bytes;
            true
        } else {
            dropped = dropped.saturating_add(1);
            false
        }
    });
    dropped
}

fn retain_fitting_records(values: &mut Vec<EncodedRecord>, remaining: &mut u64) -> u64 {
    let mut dropped = 0_u64;
    values.retain(|value| {
        if value.durability == 0 {
            return true;
        }
        let bytes = record_bytes(value).unwrap_or(u64::MAX);
        if bytes <= *remaining {
            *remaining -= bytes;
            true
        } else {
            dropped = dropped.saturating_add(1);
            false
        }
    });
    dropped
}

fn critical_write_bytes(write: &EncodedWrite) -> Result<u64, StorageError> {
    let events = write
        .events
        .iter()
        .try_fold(0_u64, |sum, value| add(sum, event_bytes(value)?))?;
    let deliveries = write
        .deliveries
        .iter()
        .filter(|value| value.durability == 0)
        .try_fold(0_u64, |sum, value| add(sum, delivery_bytes(value)?))?;
    write
        .records
        .iter()
        .filter(|value| value.durability == 0)
        .try_fold(add(events, deliveries)?, |sum, value| {
            add(sum, record_bytes(value)?)
        })
}

fn event_bytes(value: &EncodedEvent) -> Result<u64, StorageError> {
    strings(&[&value.event_id, &value.resource, &value.payload], 24)
}

fn delivery_bytes(value: &EncodedDelivery) -> Result<u64, StorageError> {
    strings(
        &[
            &value.target_instance_id,
            &value.event_id,
            &value.delivery_id,
            &value.ordering_key,
            &value.deadline,
            &value.payload,
        ],
        40,
    )
}

fn record_bytes(value: &EncodedRecord) -> Result<u64, StorageError> {
    strings(&[&value.record_id, &value.committed_at, &value.payload], 24)
}

fn strings(values: &[&str], overhead: u64) -> Result<u64, StorageError> {
    values.iter().try_fold(overhead, |sum, value| {
        add(
            sum,
            u64::try_from(value.len()).map_err(|_| capacity_error())?,
        )
    })
}

fn used_bytes(connection: &Connection) -> Result<u64, StorageError> {
    let statement = "SELECT
        COALESCE((SELECT SUM(length(CAST(event_id AS BLOB)) +
                                   length(CAST(resource AS BLOB)) +
                                   length(CAST(payload AS BLOB)) + 24)
                  FROM journal_events), 0) +
        COALESCE((SELECT SUM(length(CAST(target_instance_id AS BLOB)) +
                                   length(CAST(event_id AS BLOB)) +
                                   length(CAST(delivery_id AS BLOB)) +
                                   length(CAST(ordering_key AS BLOB)) +
                                   length(CAST(deadline AS BLOB)) +
                                   length(CAST(payload AS BLOB)) + 40)
                  FROM target_deliveries), 0) +
        COALESCE((SELECT SUM(length(CAST(delivery_id AS BLOB)) +
                                   length(CAST(outcome AS BLOB)) +
                                   length(CAST(reported_at AS BLOB)) +
                                   COALESCE(length(CAST(retry_at AS BLOB)), 0) + 24)
                  FROM target_delivery_attempts), 0) +
        COALESCE((SELECT SUM(length(CAST(record_id AS BLOB)) +
                                   length(CAST(committed_at AS BLOB)) +
                                   length(CAST(payload AS BLOB)) + 24)
                  FROM committed_records), 0)";
    scalar(connection, statement)
}

fn scalar(connection: &Connection, statement: &str) -> Result<u64, StorageError> {
    let value = connection
        .query_row(statement, [], |row| row.get::<_, i64>(0))
        .map_err(unavailable)?;
    u64::try_from(value).map_err(|_| capacity_error())
}

fn stat(connection: &Connection, category: &str) -> Result<u64, StorageError> {
    let value = connection
        .query_row(
            "SELECT COALESCE((SELECT count FROM storage_retention_stats WHERE category = ?1), 0)",
            [category],
            |row| row.get::<_, i64>(0),
        )
        .map_err(unavailable)?;
    u64::try_from(value).map_err(|_| capacity_error())
}

fn increment_stat(
    transaction: &Transaction<'_>,
    category: &str,
    amount: u64,
) -> Result<(), StorageError> {
    if amount == 0 {
        return Ok(());
    }
    let amount = i64::try_from(amount).map_err(|_| capacity_error())?;
    transaction
        .execute(
            "INSERT INTO storage_retention_stats(category, count) VALUES (?1, ?2)
             ON CONFLICT(category) DO UPDATE SET count = count + excluded.count",
            params![category, amount],
        )
        .map(|_| ())
        .map_err(unavailable)
}

fn count(value: usize) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| capacity_error())
}

fn add(left: u64, right: u64) -> Result<u64, StorageError> {
    left.checked_add(right).ok_or_else(capacity_error)
}

fn capacity_error() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "operational storage accounting exceeds supported range",
    )
}
