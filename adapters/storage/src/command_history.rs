use rusqlite::{Connection, OptionalExtension, params};
use serde::de::IgnoredAny;
use sha2::{Digest, Sha256};
use uob_application::{
    COMMAND_HISTORY_CURSOR_PREFIX, CommandHistoryCursor, CommandHistoryQuery, CommandHistoryScope,
    Page, StorageError, StorageErrorCode,
};
use uob_contracts::{
    Command, CommandOperation, CommandOperationKind, CommandResult, CommandSummary,
};

use crate::configuration::unavailable;

pub(crate) fn read(
    connection: &Connection,
    query: &CommandHistoryQuery,
    scope: &CommandHistoryScope,
) -> Result<Page<CommandSummary, CommandHistoryCursor>, StorageError> {
    let station = crate::snapshots::station_key(&query.station)?;
    if scope.is_empty() {
        return Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        });
    }
    let mut children = scope
        .resources
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid_json)?;
    children.sort_unstable();
    children.dedup();
    let children = format!("[{}]", children.join(","));
    let scope_key = format!(
        "{station}\n{}\n{}\n{children}",
        scope.descendants, scope.station_only
    );
    let digest = hash_hex(&scope_key);
    let anchor = query
        .after
        .as_ref()
        .map(|cursor| {
            let position = decode_cursor(cursor, &digest)?;
            let stored = connection
                .query_row(
                    "SELECT request_id, admitted_at, payload FROM commands WHERE rowid = ?1",
                    [position.rowid],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(unavailable)?
                .ok_or_else(expired_cursor)?;
            let (request_id, admitted_at, payload) = stored;
            if admitted_at != position.admitted_at
                || hash_hex(&request_id) != position.request_digest
                || hash_hex(&payload) != position.payload_digest
            {
                return Err(expired_cursor());
            }
            let command: Command<IgnoredAny> =
                serde_json::from_str(&payload).map_err(invalid_json)?;
            if command.request_id.as_str() != request_id
                || !scope.permits(&command.resource, &query.station)
            {
                return Err(expired_cursor());
            }
            Ok((admitted_at, request_id))
        })
        .transpose()?;
    let (at, id) = anchor.unwrap_or((i64::MAX, String::new()));
    let mut rows = fetch_rows(connection, query, scope, &children, at, &id)?;
    let more = rows.len() > usize::from(query.limit.get());
    rows.truncate(usize::from(query.limit.get()));
    let next_cursor = if more {
        rows.last()
            .map(|(rowid, admitted_at, request_id, command, _)| {
                encode_cursor(&digest, *rowid, *admitted_at, request_id, command)
            })
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|(_, _, request_id, command, result)| {
            materialize_row(&request_id, &command, result.as_deref(), query, scope)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page { items, next_cursor })
}

type HistoryRow = (i64, i64, String, String, Option<String>);

fn fetch_rows(
    connection: &Connection,
    query: &CommandHistoryQuery,
    scope: &CommandHistoryScope,
    children: &str,
    at: i64,
    id: &str,
) -> Result<Vec<HistoryRow>, StorageError> {
    // Both canonical station identity and exact child-resource grants precede LIMIT in SQL.
    // A left join retains commands whose first lifecycle result has not yet been written.
    let mut statement = connection.prepare(
        "SELECT c.rowid, c.admitted_at, c.request_id, c.payload, r.payload FROM commands c
         LEFT JOIN command_results r ON r.request_id = c.request_id
         WHERE json_extract(c.payload, '$.resource.bridge_id') = ?1
           AND json_extract(c.payload, '$.resource.station_id') = ?2
           AND (?3 = 1 OR
                (json_type(c.payload, '$.resource.resource') IS NULL AND ?4 = 1) OR
                EXISTS (SELECT 1 FROM json_each(?5) AS grant_child
                        WHERE json(grant_child.value) = json(json_extract(c.payload, '$.resource.resource'))))
           AND (c.admitted_at < ?6 OR (c.admitted_at = ?6 AND c.request_id < ?7))
         ORDER BY c.admitted_at DESC, c.request_id DESC LIMIT ?8",
    ).map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![
                query.station.bridge_id.as_str(),
                query.station.station_id.as_str(),
                i64::from(scope.descendants),
                i64::from(scope.station_only),
                children,
                at,
                id,
                i64::from(query.limit.get()) + 1
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(unavailable)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
}

fn materialize_row(
    request_id: &str,
    command: &str,
    result: Option<&str>,
    query: &CommandHistoryQuery,
    scope: &CommandHistoryScope,
) -> Result<CommandSummary, StorageError> {
    let command: Command<IgnoredAny> = serde_json::from_str(command).map_err(invalid_json)?;
    if command.request_id.as_str() != request_id
        || !scope.permits(&command.resource, &query.station)
    {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "command history resource mismatch",
        ));
    }
    let result: Option<CommandResult> = result
        .map(serde_json::from_str)
        .transpose()
        .map_err(invalid_json)?;
    if result.as_ref().is_some_and(|result| {
        result.return_route.request_id != command.request_id || result.resource != command.resource
    }) {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "command history result mismatch",
        ));
    }
    let operation = match command.operation {
        CommandOperation::Start { .. } => CommandOperationKind::Start,
        CommandOperation::Stop { .. } => CommandOperationKind::Stop,
        CommandOperation::SetChargingLimit(_) => CommandOperationKind::SetChargingLimit,
        CommandOperation::Ocpp(_) => CommandOperationKind::Ocpp,
    };
    Ok(CommandSummary {
        request_id: command.request_id,
        correlation_id: command.correlation_id,
        resource: command.resource,
        operation,
        admitted_at: command.admitted_at,
        expires_at: command.expires_at,
        lifecycle: result.as_ref().map(|result| result.lifecycle.clone()),
        recorded_at: result.as_ref().map(|result| result.recorded_at),
        observed_effects: result.map_or_else(Vec::new, |result| result.observed_effects),
    })
}

struct CursorPosition<'a> {
    rowid: i64,
    admitted_at: i64,
    request_digest: &'a str,
    payload_digest: &'a str,
}

fn hash_hex(value: &str) -> String {
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(value.as_bytes()) {
        use std::fmt::Write;
        write!(&mut digest, "{byte:02x}").expect("write to String");
    }
    digest
}

fn encode_cursor(
    scope_digest: &str,
    rowid: i64,
    admitted_at: i64,
    request_id: &str,
    command: &str,
) -> Result<CommandHistoryCursor, StorageError> {
    CommandHistoryCursor::new(format!(
        "{COMMAND_HISTORY_CURSOR_PREFIX}{scope_digest}:{rowid}:{admitted_at}:{}:{}",
        hash_hex(request_id),
        hash_hex(command)
    ))
}

fn decode_cursor<'a>(
    cursor: &'a CommandHistoryCursor,
    scope_digest: &str,
) -> Result<CursorPosition<'a>, StorageError> {
    let suffix = cursor
        .as_str()
        .strip_prefix(COMMAND_HISTORY_CURSOR_PREFIX)
        .ok_or_else(invalid_cursor)?;
    let mut parts = suffix.split(':');
    let scope = parts.next().ok_or_else(invalid_cursor)?;
    let rowid = parts
        .next()
        .ok_or_else(invalid_cursor)?
        .parse::<i64>()
        .map_err(|_| invalid_cursor())?;
    let admitted_at = parts
        .next()
        .ok_or_else(invalid_cursor)?
        .parse::<i64>()
        .map_err(|_| invalid_cursor())?;
    let request_digest = parts.next().ok_or_else(invalid_cursor)?;
    let payload_digest = parts.next().ok_or_else(invalid_cursor)?;
    if scope != scope_digest
        || rowid <= 0
        || parts.next().is_some()
        || !valid_digest(request_digest)
        || !valid_digest(payload_digest)
    {
        return Err(invalid_cursor());
    }
    Ok(CursorPosition {
        rowid,
        admitted_at,
        request_digest,
        payload_digest,
    })
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn expired_cursor() -> StorageError {
    StorageError::new(
        StorageErrorCode::CursorExpired,
        "command history cursor expired",
    )
}

fn invalid_cursor() -> StorageError {
    StorageError::new(
        StorageErrorCode::InvalidRequest,
        "invalid command history cursor",
    )
}

fn invalid_json(_: serde_json::Error) -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "command history could not be decoded",
    )
}
