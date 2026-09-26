use rusqlite::{Connection, params};
use serde::de::DeserializeOwned;
use uob_application::{
    CommittedRecord, CommittedRecordCursor, Page, RETAINED_EVENT_CURSOR_PREFIX,
    RetainedEventCursor, RetainedEventPage, StorageError, StorageErrorCode,
};

use crate::{codec, configuration::unavailable};

pub(super) fn read_events<E: DeserializeOwned>(
    connection: &Connection,
    resource: &str,
    after: Option<i64>,
    limit: usize,
) -> Result<RetainedEventPage<E>, StorageError> {
    if let Some(cursor) = after {
        let retained = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM journal_events WHERE resource = ?1 AND row_id = ?2)",
                params![resource, cursor],
                |row| row.get::<_, bool>(0),
            )
            .map_err(unavailable)?;
        if !retained {
            return Err(StorageError::new(
                StorageErrorCode::CursorExpired,
                "durable event cursor expired; fetch a fresh snapshot",
            ));
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT row_id, payload FROM journal_events WHERE resource = ?1 AND row_id > ?2\n\
             ORDER BY row_id LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![resource, after.unwrap_or(0), limit_plus_one(limit)?],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(unavailable)?;
    let mut values = collect_rows(rows)?;
    let has_more = values.len() > limit;
    values.truncate(limit);
    let resume_position = values.last().map_or(after, |value| Some(value.0));
    let resume_cursor = resume_position
        .map(|position| {
            RetainedEventCursor::new(format!("{RETAINED_EVENT_CURSOR_PREFIX}{position}"))
        })
        .transpose()?;
    let events = values
        .into_iter()
        .map(|(_, payload)| codec::decode_event(&payload))
        .collect::<Result<_, _>>()?;
    Ok(RetainedEventPage {
        events,
        resume_cursor,
        has_more,
    })
}

pub(super) fn read_records<R: DeserializeOwned>(
    connection: &Connection,
    after: Option<i64>,
    limit: usize,
    include_telemetry: bool,
) -> Result<Page<CommittedRecord<R>, CommittedRecordCursor>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT row_id, record_id, durability, committed_at, payload\n\
             FROM committed_records WHERE row_id > ?1 AND (?2 OR durability = 0)\n\
             ORDER BY row_id LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![
                after.unwrap_or(0),
                include_telemetry,
                limit_plus_one(limit)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(unavailable)?;
    let mut values = collect_rows(rows)?;
    let has_more = values.len() > limit;
    values.truncate(limit);
    let next_cursor = has_more
        .then(|| {
            values
                .last()
                .map(|value| CommittedRecordCursor::new(value.0.to_string()))
        })
        .flatten()
        .transpose()?;
    let items = values
        .into_iter()
        .map(|(_, id, durability, at, payload)| codec::decode_record(id, durability, &at, &payload))
        .collect::<Result<_, _>>()?;
    Ok(Page { items, next_cursor })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, StorageError> {
    rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)
}

fn limit_plus_one(limit: usize) -> Result<i64, StorageError> {
    limit_i64(limit.saturating_add(1))
}
fn limit_i64(limit: usize) -> Result<i64, StorageError> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "page limit exceeds SQLite integer range",
        )
    })
}
