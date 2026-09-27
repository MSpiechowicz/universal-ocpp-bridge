use rusqlite::{Connection, MAIN_DB, OptionalExtension, params};
use serde::de::DeserializeOwned;
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    sync::Arc,
};
use uob_application::{
    BudgetedRecordChunk, CommittedRecordChunkQuery, CommittedRecordChunkResult,
    CommittedRecordCursor, CommittedRecordDescriptor, CommittedRecordField, CommittedRecordPage,
    CommittedRecordQuery, CommittedRecordReadToken, Durability, RETAINED_EVENT_CURSOR_PREFIX,
    RetainedEventCursor, RetainedEventPage, RuntimeReservation, StorageError, StorageErrorCode,
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

const SOURCE_CURSOR_PREFIX: &str = "uob:record:v1:";

pub(super) fn read_records(
    connection: &Connection,
    query: &CommittedRecordQuery,
    reservation: RuntimeReservation,
    token_key: &RandomState,
) -> Result<CommittedRecordPage, StorageError> {
    let transaction = connection.unchecked_transaction().map_err(unavailable)?;
    let stream = stream(query.durability);
    let (generation, legacy): (String, bool) = transaction
        .query_row(
            "SELECT generation, legacy_baseline_incomplete FROM committed_source_identity WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    if generation.len() > 128 {
        return Err(integrity());
    }
    let (high_water, expired_prefix): (i64, i64) = transaction
        .query_row(
            "SELECT high_water, expired_prefix FROM committed_source_streams WHERE durability = ?1",
            [stream],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    if high_water < 0 || expired_prefix < 0 || expired_prefix > high_water {
        return Err(integrity());
    }
    let prefix = format!("{SOURCE_CURSOR_PREFIX}{generation}:{stream}:");
    let after = match query.after.as_ref() {
        None => 0,
        Some(cursor) => cursor
            .as_str()
            .strip_prefix(&prefix)
            .and_then(|position| position.parse::<i64>().ok())
            .filter(|position| *position >= 0 && *position <= high_water)
            .ok_or_else(|| {
                StorageError::new(
                    StorageErrorCode::CursorExpired,
                    "committed-record cursor is outside the retained source stream",
                )
            })?,
    };

    let (items, last, has_more) = read_descriptors(
        &transaction,
        query,
        token_key,
        &generation,
        &prefix,
        after,
        high_water,
    )?;
    let checkpoint = if has_more { last } else { high_water };
    let lost_records = checkpoint
        .checked_sub(after)
        .and_then(|span| span.checked_sub(i64::try_from(items.len()).ok()?))
        .ok_or_else(integrity)?;
    transaction.commit().map_err(unavailable)?;
    Ok(CommittedRecordPage {
        items,
        source_generation: generation,
        resume_cursor: CommittedRecordCursor::new(format!("{prefix}{checkpoint}"))?,
        has_more,
        high_water: high_water.cast_unsigned(),
        expired_prefix: expired_prefix.cast_unsigned(),
        lost_records: lost_records.cast_unsigned(),
        legacy_baseline_incomplete: legacy,
        reservation: Some(reservation),
    })
}

fn read_descriptors(
    connection: &Connection,
    query: &CommittedRecordQuery,
    token_key: &RandomState,
    generation: &str,
    prefix: &str,
    after: i64,
    high_water: i64,
) -> Result<(Vec<CommittedRecordDescriptor>, i64, bool), StorageError> {
    let stream = stream(query.durability);
    let token_generation: Arc<str> = Arc::from(generation);
    let mut statement = connection
        .prepare(
            "SELECT row_id, source_sequence, octet_length(record_id),
                    octet_length(committed_at), octet_length(payload)
             FROM committed_records
             WHERE durability = ?1 AND source_sequence > ?2
             ORDER BY source_sequence LIMIT ?3",
        )
        .map_err(unavailable)?;
    let mut rows = statement
        .query(params![
            stream,
            after,
            limit_plus_one(usize::from(query.limit.get()))?
        ])
        .map_err(unavailable)?;
    let mut items = Vec::new();
    let mut last = after;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(unavailable)? {
        if items.len() == usize::from(query.limit.get()) {
            has_more = true;
            break;
        }
        let row_id: i64 = row.get(0).map_err(unavailable)?;
        let sequence: i64 = row.get(1).map_err(unavailable)?;
        let lengths = row_lengths(row, 2)?;
        if row_id <= 0 || sequence <= last || sequence > high_water {
            return Err(integrity());
        }
        let cursor = CommittedRecordCursor::new(format!("{prefix}{sequence}"))?;
        let authenticator = authenticate(
            token_key,
            generation,
            stream,
            sequence.cast_unsigned(),
            row_id,
            lengths,
        );
        let token = CommittedRecordReadToken::new(
            Arc::clone(&token_generation),
            query.durability,
            sequence.cast_unsigned(),
            row_id,
            lengths,
            authenticator,
        );
        items.push(CommittedRecordDescriptor {
            token,
            durability: query.durability,
            sequence: sequence.cast_unsigned(),
            cursor,
            record_id_len: lengths[0],
            committed_at_len: lengths[1],
            payload_len: lengths[2],
        });
        last = sequence;
    }
    Ok((items, last, has_more))
}

pub(super) fn read_record_chunk(
    connection: &Connection,
    query: &CommittedRecordChunkQuery,
    reservation: RuntimeReservation,
    token_key: &RandomState,
) -> Result<CommittedRecordChunkResult, StorageError> {
    let token = &query.token;
    let sequence = i64::try_from(token.sequence()).map_err(|_| invalid_chunk())?;
    let field_len = token.lengths()[query.field.index()];
    if sequence <= 0
        || token.row_id() <= 0
        || query.max_bytes == 0
        || query.max_bytes > uob_application::EXPORT_RECORD_CHUNK_BYTES
        || query.max_bytes > reservation.bytes()
        || query.offset > field_len
        || query.offset.checked_add(query.max_bytes as u64).is_none()
    {
        return Err(invalid_chunk());
    }
    if token.authenticator()
        != authenticate(
            token_key,
            token.generation(),
            stream(token.durability()),
            token.sequence(),
            token.row_id(),
            token.lengths(),
        )
    {
        return Err(invalid_chunk());
    }
    let transaction = connection.unchecked_transaction().map_err(unavailable)?;
    let generation: String = transaction
        .query_row(
            "SELECT generation FROM committed_source_identity WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if generation != token.generation() {
        return Err(invalid_chunk());
    }
    let stream = stream(token.durability());
    let high_water: i64 = transaction
        .query_row(
            "SELECT high_water FROM committed_source_streams WHERE durability = ?1",
            [stream],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if sequence > high_water || high_water < 0 {
        return Err(invalid_chunk());
    }
    let identity = transaction
        .query_row(
            "SELECT row_id, octet_length(record_id), octet_length(committed_at),
                octet_length(payload) FROM committed_records
         WHERE durability = ?1 AND source_sequence = ?2",
            params![stream, sequence],
            |row| Ok((row.get::<_, i64>(0)?, row_lengths_sql(row, 1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((row_id, lengths)) = identity else {
        transaction.commit().map_err(unavailable)?;
        return Ok(CommittedRecordChunkResult::Expired);
    };
    if row_id != token.row_id() || lengths != token.lengths() {
        return Err(integrity());
    }
    let column = match query.field {
        CommittedRecordField::RecordId => "record_id",
        CommittedRecordField::CommittedAt => "committed_at",
        CommittedRecordField::Payload => "payload",
    };
    let count = usize::try_from(field_len - query.offset)
        .unwrap_or(usize::MAX)
        .min(query.max_bytes);
    let mut bytes = vec![0; count];
    {
        let blob = transaction
            .blob_open(MAIN_DB, "committed_records", column, row_id, true)
            .map_err(unavailable)?;
        if u64::try_from(blob.len()).map_err(|_| integrity())? != field_len {
            return Err(integrity());
        }
        let offset = usize::try_from(query.offset).map_err(|_| invalid_chunk())?;
        blob.read_at_exact(&mut bytes, offset)
            .map_err(unavailable)?;
    }
    transaction.commit().map_err(unavailable)?;
    Ok(CommittedRecordChunkResult::Data(BudgetedRecordChunk::new(
        query.field,
        query.offset,
        bytes,
        field_len,
        reservation,
    )?))
}

fn authenticate(
    key: &RandomState,
    generation: &str,
    stream: i64,
    sequence: u64,
    row_id: i64,
    lengths: [u64; 3],
) -> u64 {
    let mut hasher = key.build_hasher();
    generation.hash(&mut hasher);
    stream.hash(&mut hasher);
    sequence.hash(&mut hasher);
    row_id.hash(&mut hasher);
    lengths.hash(&mut hasher);
    hasher.finish()
}

fn stream(durability: Durability) -> i64 {
    match durability {
        Durability::Critical => 0,
        Durability::BestEffortTelemetry => 1,
    }
}

fn row_lengths(row: &rusqlite::Row<'_>, start: usize) -> Result<[u64; 3], StorageError> {
    row_lengths_sql(row, start).map_err(|_| integrity())
}

fn row_lengths_sql(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<[u64; 3]> {
    let mut lengths = [0; 3];
    for (index, length) in lengths.iter_mut().enumerate() {
        let value: i64 = row.get(start + index)?;
        *length = u64::try_from(value)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(start + index, value))?;
    }
    Ok(lengths)
}

fn integrity() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "committed source row identity is inconsistent",
    )
}

fn invalid_chunk() -> StorageError {
    StorageError::new(
        StorageErrorCode::InvalidRequest,
        "invalid committed source chunk identity or range",
    )
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
