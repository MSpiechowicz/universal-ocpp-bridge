use rusqlite::{Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use uob_application::{
    AuthorizationChange, PendingDelivery, RecoveryBatch, StorageError, StorageErrorCode,
};
use uob_contracts::Command;

use crate::{codec, configuration::unavailable};

pub(crate) fn recover<C: DeserializeOwned, D: DeserializeOwned>(
    connection: &Connection,
    limit: usize,
) -> Result<RecoveryBatch<C, D>, StorageError> {
    let station_snapshots = query_json(
        connection,
        "SELECT payload FROM station_snapshots ORDER BY station_key",
    )?
    .iter()
    .map(|value| codec::decode_snapshot(value))
    .collect::<Result<_, _>>()?;
    let authorization = read_authorization(connection)?;
    let active_commands = query_json_limit(
        connection,
        "SELECT payload FROM commands WHERE unresolved = 1 ORDER BY request_id LIMIT ?1",
        limit,
    )?
    .iter()
    .map(|value| codec::decode_command(value))
    .collect::<Result<_, _>>()?;
    let command_results = query_json_limit(
        connection,
        "SELECT command_results.payload FROM command_results\n\
         JOIN commands USING(request_id) WHERE commands.unresolved = 1\n\
         ORDER BY command_results.request_id LIMIT ?1",
        limit,
    )?
    .iter()
    .map(|value| codec::decode_result(value))
    .collect::<Result<_, _>>()?;
    let pending_deliveries = read_deliveries(connection, limit)?;
    Ok(RecoveryBatch {
        station_snapshots,
        authorization,
        active_commands,
        command_results,
        pending_deliveries,
        has_more: count_unresolved_commands(connection)? > limit
            || count(connection, "target_deliveries")? > limit,
    })
}

pub(crate) fn command_result(
    connection: &Connection,
    request_id: &str,
) -> Result<Option<uob_contracts::CommandResult>, StorageError> {
    let value = connection
        .query_row(
            "SELECT payload FROM command_results WHERE request_id = ?1",
            [request_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?;
    value
        .map(|payload| codec::decode_result(&payload))
        .transpose()
}

pub(crate) fn command<C: DeserializeOwned>(
    connection: &Connection,
    request_id: &str,
) -> Result<Option<Command<C>>, StorageError> {
    let value = connection
        .query_row(
            "SELECT payload FROM commands WHERE request_id = ?1",
            [request_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?;
    value
        .map(|payload| codec::decode_command(&payload))
        .transpose()
}

fn read_authorization(connection: &Connection) -> Result<Vec<AuthorizationChange>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT reference, resource, state, revision, changed_at, expires_at FROM authorization_changes ORDER BY reference",
    ).map_err(unavailable)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(unavailable)?;
    collect_rows(rows)?
        .into_iter()
        .map(|(a, b, c, d, e, f)| codec::decode_authorization(a, &b, c, d, &e, f.as_deref()))
        .collect()
}

fn read_deliveries<D: DeserializeOwned>(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<PendingDelivery<D>>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT target_instance_id, target_revision, event_id, delivery_id, ordering_key, deadline, durability, payload\n\
         FROM target_deliveries ORDER BY target_instance_id, target_revision, event_id LIMIT ?1",
    ).map_err(unavailable)?;
    let rows = statement
        .query_map([limit_i64(limit)?], |row| {
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
        })
        .map_err(unavailable)?;
    collect_rows(rows)?
        .into_iter()
        .map(|(a, b, c, d, e, f, g, h)| {
            let value = crate::codec::EncodedDelivery {
                target_instance_id: a,
                target_revision: b,
                event_id: c,
                delivery_id: d,
                ordering_key: e,
                deadline: f,
                durability: g,
                payload: h,
            };
            codec::decode_delivery(&value)
        })
        .collect()
}

fn query_json(connection: &Connection, sql: &str) -> Result<Vec<String>, StorageError> {
    let mut statement = connection.prepare(sql).map_err(unavailable)?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .map_err(unavailable)?;
    collect_rows(rows)
}

fn query_json_limit(
    connection: &Connection,
    sql: &str,
    limit: usize,
) -> Result<Vec<String>, StorageError> {
    let mut statement = connection.prepare(sql).map_err(unavailable)?;
    let rows = statement
        .query_map([limit_i64(limit)?], |row| row.get(0))
        .map_err(unavailable)?;
    collect_rows(rows)
}

fn count(connection: &Connection, table: &str) -> Result<usize, StorageError> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(unavailable)?;
    usize::try_from(count)
        .map_err(|_| StorageError::new(StorageErrorCode::IntegrityFailure, "negative row count"))
}

fn count_unresolved_commands(connection: &Connection) -> Result<usize, StorageError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM commands WHERE unresolved = 1",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    usize::try_from(count)
        .map_err(|_| StorageError::new(StorageErrorCode::IntegrityFailure, "negative row count"))
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
            "recovery limit is too large",
        )
    })
}
