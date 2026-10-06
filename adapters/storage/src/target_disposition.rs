//! Per-destination outbox backlog and audited, start-up-only archive/discard execution.
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_application::{
    DeliveryDispositionAction, DeliveryDispositionRecord, DeliveryDispositionRequest,
    DeliveryDispositionState, MAX_PENDING_TARGET_DISPOSITIONS, MAX_TARGET_BACKLOG_DESTINATIONS,
    StorageError, StorageErrorCode, StorageFuture, TargetBacklogFact, TargetDeliveryDestination,
    TargetDispositionStore,
};
use uob_contracts::{PrincipalId, TargetInstanceId, UtcTimestamp};

use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};

/// `durability` column value of critical outbox rows.
const CRITICAL: i64 = 0;

impl<C, E, D, R> TargetDispositionStore for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn target_delivery_backlog(&self) -> StorageFuture<'_, Vec<TargetBacklogFact>> {
        self.request(Request::TargetBacklog)
    }

    fn pending_target_dispositions(&self) -> StorageFuture<'_, Vec<DeliveryDispositionRecord>> {
        self.request(Request::PendingTargetDispositions)
    }

    fn authorize_target_disposition(
        &self,
        request: DeliveryDispositionRequest,
    ) -> StorageFuture<'_, DeliveryDispositionRecord> {
        self.request(|reply| Request::AuthorizeTargetDisposition(request, reply))
    }

    fn settle_target_dispositions(
        &self,
        selected: Option<TargetDeliveryDestination>,
        settled_at: UtcTimestamp,
    ) -> StorageFuture<'_, Vec<DeliveryDispositionRecord>> {
        self.request(|reply| Request::SettleTargetDispositions(selected, settled_at, reply))
    }
}

pub(crate) fn create(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS target_delivery_dispositions(
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 audit_event_id TEXT NOT NULL UNIQUE,
                 target_instance_id TEXT NOT NULL,
                 target_revision INTEGER NOT NULL CHECK (target_revision >= 0),
                 action TEXT NOT NULL CHECK (action IN ('archive', 'discard')),
                 principal_id TEXT NOT NULL,
                 authorized_at TEXT NOT NULL,
                 state TEXT NOT NULL CHECK (state IN ('authorized', 'executed', 'superseded')),
                 settled_at TEXT,
                 critical_deliveries INTEGER CHECK (critical_deliveries >= 0),
                 deliveries INTEGER CHECK (deliveries >= 0)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS target_delivery_dispositions_authorized
                 ON target_delivery_dispositions(target_instance_id, target_revision)
                 WHERE state = 'authorized';
             CREATE TABLE IF NOT EXISTS target_delivery_archive(
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 audit_event_id TEXT NOT NULL
                     REFERENCES target_delivery_dispositions(audit_event_id),
                 target_instance_id TEXT NOT NULL,
                 target_revision INTEGER NOT NULL CHECK (target_revision >= 0),
                 event_id TEXT NOT NULL, delivery_id TEXT NOT NULL, ordering_key TEXT NOT NULL,
                 deadline TEXT NOT NULL, durability INTEGER NOT NULL, payload TEXT NOT NULL,
                 attempt_count INTEGER NOT NULL CHECK (attempt_count >= 0),
                 archived_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS target_delivery_archive_audit
                 ON target_delivery_archive(audit_event_id, row_id);",
        )
        .map_err(unavailable)
}

pub(crate) fn backlog(connection: &Connection) -> Result<Vec<TargetBacklogFact>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT target_instance_id, target_revision,
                    SUM(CASE WHEN durability = ?1 THEN 1 ELSE 0 END), COUNT(*)
             FROM target_deliveries
             GROUP BY target_instance_id, target_revision
             ORDER BY target_instance_id, target_revision
             LIMIT ?2",
        )
        .map_err(unavailable)?;
    let limit = i64::try_from(MAX_TARGET_BACKLOG_DESTINATIONS + 1).map_err(|_| capacity())?;
    let rows = statement
        .query_map(params![CRITICAL, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    if rows.len() > MAX_TARGET_BACKLOG_DESTINATIONS {
        return Err(capacity());
    }
    rows.into_iter()
        .map(|(target, revision, critical, total)| {
            Ok(TargetBacklogFact {
                destination: destination(target, revision)?,
                pending_critical_deliveries: count(critical)?,
                pending_deliveries: count(total)?,
            })
        })
        .collect()
}

pub(crate) fn pending(
    connection: &Connection,
) -> Result<Vec<DeliveryDispositionRecord>, StorageError> {
    read_records(connection, "WHERE state = 'authorized'")
}

pub(crate) fn authorize(
    connection: &mut Connection,
    request: &DeliveryDispositionRequest,
) -> Result<DeliveryDispositionRecord, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let target = request.destination.target_instance_id.as_str();
    let revision = revision(request.destination.configuration_revision)?;
    let (critical, _) = destination_counts(&transaction, target, revision)?;
    if critical == 0 {
        return Err(StorageError::new(
            StorageErrorCode::Conflict,
            "destination has no pending critical deliveries",
        ));
    }
    let existing = transaction
        .query_row(
            "SELECT 1 FROM target_delivery_dispositions
             WHERE state = 'authorized' AND target_instance_id = ?1 AND target_revision = ?2",
            params![target, revision],
            |_| Ok(()),
        )
        .optional()
        .map_err(unavailable)?;
    if existing.is_some() {
        return Err(StorageError::new(
            StorageErrorCode::Conflict,
            "destination already has an unsettled disposition",
        ));
    }
    let unsettled: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM target_delivery_dispositions WHERE state = 'authorized'",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if usize::try_from(unsettled).map_err(|_| capacity())? >= MAX_PENDING_TARGET_DISPOSITIONS {
        return Err(capacity());
    }
    let audit_event_id: String = transaction
        .query_row(
            "SELECT 'disposition-' || lower(hex(randomblob(16)))",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "INSERT INTO target_delivery_dispositions(
                 audit_event_id, target_instance_id, target_revision, action, principal_id,
                 authorized_at, state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'authorized')",
            params![
                audit_event_id,
                target,
                revision,
                action_code(request.action),
                request.principal_id.as_str(),
                timestamp(request.authorized_at)?,
            ],
        )
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)?;
    Ok(DeliveryDispositionRecord {
        audit_event_id,
        destination: request.destination.clone(),
        action: request.action,
        principal_id: request.principal_id.clone(),
        authorized_at: request.authorized_at,
        state: DeliveryDispositionState::Authorized,
        settled_at: None,
        critical_deliveries: None,
        deliveries: None,
    })
}

pub(crate) fn settle(
    connection: &mut Connection,
    selected: Option<&TargetDeliveryDestination>,
    settled_at: UtcTimestamp,
) -> Result<Vec<DeliveryDispositionRecord>, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let now = timestamp(settled_at)?;
    let authorized = read_records(&transaction, "WHERE state = 'authorized'")?;
    let mut settled = Vec::with_capacity(authorized.len());
    for mut record in authorized {
        if selected == Some(&record.destination) {
            transaction
                .execute(
                    "UPDATE target_delivery_dispositions SET state = 'superseded', settled_at = ?2
                     WHERE audit_event_id = ?1",
                    params![record.audit_event_id, now],
                )
                .map_err(unavailable)?;
            record.state = DeliveryDispositionState::Superseded;
        } else {
            let (critical, total) = execute(&transaction, &record, &now)?;
            record.state = DeliveryDispositionState::Executed;
            record.critical_deliveries = Some(critical);
            record.deliveries = Some(total);
        }
        record.settled_at = Some(settled_at);
        settled.push(record);
    }
    transaction.commit().map_err(unavailable)?;
    Ok(settled)
}

fn execute(
    transaction: &Transaction<'_>,
    record: &DeliveryDispositionRecord,
    now: &str,
) -> Result<(u64, u64), StorageError> {
    let target = record.destination.target_instance_id.as_str();
    let revision = revision(record.destination.configuration_revision)?;
    let (critical, total) = destination_counts(transaction, target, revision)?;
    if record.action == DeliveryDispositionAction::Archive {
        transaction
            .execute(
                "INSERT INTO target_delivery_archive(
                     audit_event_id, target_instance_id, target_revision, event_id, delivery_id,
                     ordering_key, deadline, durability, payload, attempt_count, archived_at
                 )
                 SELECT ?3, target_instance_id, target_revision, event_id, delivery_id,
                        ordering_key, deadline, durability, payload, attempt_count, ?4
                 FROM target_deliveries
                 WHERE target_instance_id = ?1 AND target_revision = ?2
                 ORDER BY rowid",
                params![target, revision, record.audit_event_id, now],
            )
            .map_err(unavailable)?;
    }
    transaction
        .execute(
            "DELETE FROM target_deliveries WHERE target_instance_id = ?1 AND target_revision = ?2",
            params![target, revision],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE target_delivery_dispositions
             SET state = 'executed', settled_at = ?2, critical_deliveries = ?3, deliveries = ?4
             WHERE audit_event_id = ?1",
            params![
                record.audit_event_id,
                now,
                i64::try_from(critical).map_err(|_| capacity())?,
                i64::try_from(total).map_err(|_| capacity())?,
            ],
        )
        .map_err(unavailable)?;
    Ok((critical, total))
}

fn destination_counts(
    connection: &Connection,
    target: &str,
    revision: i64,
) -> Result<(u64, u64), StorageError> {
    let (critical, total): (i64, i64) = connection
        .query_row(
            "SELECT COALESCE(SUM(CASE WHEN durability = ?3 THEN 1 ELSE 0 END), 0), COUNT(*)
             FROM target_deliveries WHERE target_instance_id = ?1 AND target_revision = ?2",
            params![target, revision, CRITICAL],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    Ok((count(critical)?, count(total)?))
}

fn read_records(
    connection: &Connection,
    filter: &str,
) -> Result<Vec<DeliveryDispositionRecord>, StorageError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT audit_event_id, target_instance_id, target_revision, action, principal_id,
                    authorized_at, state, settled_at, critical_deliveries, deliveries
             FROM target_delivery_dispositions {filter} ORDER BY row_id LIMIT ?1"
        ))
        .map_err(unavailable)?;
    let limit = i64::try_from(MAX_PENDING_TARGET_DISPOSITIONS).map_err(|_| capacity())?;
    let rows = statement
        .query_map([limit], |row| {
            Ok(StoredRecord {
                audit_event_id: row.get(0)?,
                target_instance_id: row.get(1)?,
                target_revision: row.get(2)?,
                action: row.get(3)?,
                principal_id: row.get(4)?,
                authorized_at: row.get(5)?,
                state: row.get(6)?,
                settled_at: row.get(7)?,
                critical_deliveries: row.get(8)?,
                deliveries: row.get(9)?,
            })
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    rows.into_iter().map(StoredRecord::decode).collect()
}

struct StoredRecord {
    audit_event_id: String,
    target_instance_id: String,
    target_revision: i64,
    action: String,
    principal_id: String,
    authorized_at: String,
    state: String,
    settled_at: Option<String>,
    critical_deliveries: Option<i64>,
    deliveries: Option<i64>,
}

impl StoredRecord {
    fn decode(self) -> Result<DeliveryDispositionRecord, StorageError> {
        Ok(DeliveryDispositionRecord {
            audit_event_id: self.audit_event_id,
            destination: destination(self.target_instance_id, self.target_revision)?,
            action: match self.action.as_str() {
                "archive" => DeliveryDispositionAction::Archive,
                "discard" => DeliveryDispositionAction::Discard,
                _ => return Err(corrupt("unknown disposition action")),
            },
            principal_id: PrincipalId::new(self.principal_id)
                .map_err(|_| corrupt("invalid disposition principal"))?,
            authorized_at: parse_timestamp(&self.authorized_at)?,
            state: match self.state.as_str() {
                "authorized" => DeliveryDispositionState::Authorized,
                "executed" => DeliveryDispositionState::Executed,
                "superseded" => DeliveryDispositionState::Superseded,
                _ => return Err(corrupt("unknown disposition state")),
            },
            settled_at: self
                .settled_at
                .as_deref()
                .map(parse_timestamp)
                .transpose()?,
            critical_deliveries: self.critical_deliveries.map(count).transpose()?,
            deliveries: self.deliveries.map(count).transpose()?,
        })
    }
}

const fn action_code(action: DeliveryDispositionAction) -> &'static str {
    match action {
        DeliveryDispositionAction::Archive => "archive",
        DeliveryDispositionAction::Discard => "discard",
    }
}

fn destination(target: String, revision: i64) -> Result<TargetDeliveryDestination, StorageError> {
    Ok(TargetDeliveryDestination {
        target_instance_id: TargetInstanceId::new(target)
            .map_err(|_| corrupt("invalid target instance in outbox"))?,
        configuration_revision: u64::try_from(revision)
            .map_err(|_| corrupt("negative target revision"))?,
    })
}

fn revision(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "target revision exceeds SQLite integer range",
        )
    })
}

fn count(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| corrupt("negative delivery count"))
}

fn timestamp(value: UtcTimestamp) -> Result<String, StorageError> {
    value.into_inner().format(&Rfc3339).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "disposition timestamp cannot be encoded",
        )
    })
}

fn parse_timestamp(value: &str) -> Result<UtcTimestamp, StorageError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(UtcTimestamp::new)
        .map_err(|_| corrupt("invalid disposition timestamp"))
}

fn capacity() -> StorageError {
    StorageError::new(
        StorageErrorCode::CapacityExhausted,
        "target disposition bound exceeded",
    )
}

fn corrupt(detail: &str) -> StorageError {
    StorageError::new(StorageErrorCode::IntegrityFailure, detail)
}
