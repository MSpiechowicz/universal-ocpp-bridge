use rusqlite::{Connection, MAIN_DB, OptionalExtension, params};
use serde::Deserialize;
use serde_json::value::RawValue;
use uob_application::{
    EXPORT_SPOOL_PAGE_BYTES, ExportSpoolError, ExportSpoolErrorCode, RuntimeResourceBudget,
    WorkClass,
};

use super::{Limits, fail};

const PENDING_V2: &str = "CREATE TABLE pending (
    row_id INTEGER PRIMARY KEY,
    durability INTEGER NOT NULL CHECK(durability IN (0,1)),
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    record_id BLOB NOT NULL,
    committed_at BLOB NOT NULL,
    payload BLOB NOT NULL,
    UNIQUE(durability,sequence)
)";

#[derive(Deserialize)]
struct LegacyRecord<'a> {
    record_id: String,
    #[serde(borrow)]
    committed_at: &'a RawValue,
    #[serde(borrow)]
    record: &'a RawValue,
}

fn integrity() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::IntegrityFailure,
        "invalid legacy export record",
    )
}

pub(super) fn configure(
    connection: &Connection,
    limits: Limits,
    budget: &RuntimeResourceBudget,
) -> Result<(), ExportSpoolError> {
    connection
        .pragma_update(None, "page_size", 4096)
        .map_err(|error| fail(&error))?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(|error| fail(&error))?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| fail(&error))?;
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(|error| fail(&error))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| fail(&error))?;
    connection
        .busy_timeout(std::time::Duration::ZERO)
        .map_err(|error| fail(&error))?;
    // The default SQLite cache can grow with a many-request transaction; keep it finite.
    connection
        .pragma_update(None, "cache_size", -2048)
        .map_err(|error| fail(&error))?;
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|error| fail(&error))?;
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|error| fail(&error))?;
    if mode != "delete" || page_size != 4096 {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::Unavailable,
            "spool requires 4096-byte pages and a bounded rollback journal",
        ));
    }
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| fail(&error))?;
    if version > 3 {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::IntegrityFailure,
            "spool schema is newer than this adapter",
        ));
    }
    let max_pages = i64::try_from(limits.main_bytes / 4096).map_err(|_| integrity())?;
    connection
        .pragma_update(None, "max_page_count", max_pages)
        .map_err(|error| fail(&error))?;
    let actual_max: i64 = connection
        .pragma_query_value(None, "max_page_count", |row| row.get(0))
        .map_err(|error| fail(&error))?;
    if actual_max > max_pages {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::Backpressure,
            "existing spool exceeds its physical main-file ceiling",
        ));
    }
    match version {
        0 => {
            initialize(connection)?;
            add_delivery(connection)?;
        }
        1 => {
            migrate(connection, budget)?;
            add_delivery(connection)?;
        }
        2 => add_delivery(connection)?,
        3 => {}
        _ => unreachable!(),
    }
    let page_count: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|error| fail(&error))?;
    if page_count > max_pages {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::Backpressure,
            "existing spool exceeds its physical main-file ceiling",
        ));
    }
    Ok(())
}

fn initialize(connection: &Connection) -> Result<(), ExportSpoolError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
        CREATE TABLE binding (
            id INTEGER PRIMARY KEY CHECK(id=1), destination TEXT NOT NULL,
            revision INTEGER NOT NULL, kind TEXT NOT NULL, generation TEXT NOT NULL,
            critical_cursor TEXT, critical_seq INTEGER NOT NULL DEFAULT 0,
            telemetry_cursor TEXT, telemetry_seq INTEGER NOT NULL DEFAULT 0,
            critical_high INTEGER NOT NULL DEFAULT 0, telemetry_high INTEGER NOT NULL DEFAULT 0,
            incomplete INTEGER NOT NULL DEFAULT 0, legacy INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE gaps (
            durability INTEGER NOT NULL, first INTEGER NOT NULL, last INTEGER NOT NULL,
            reason INTEGER NOT NULL, CHECK(first > 0 AND last >= first),
            PRIMARY KEY(durability, first)
        ) WITHOUT ROWID;",
        )
        .map_err(|error| fail(&error))?;
    let result = (|| {
        connection
            .execute_batch(PENDING_V2)
            .map_err(|error| fail(&error))?;
        connection
            .execute_batch("PRAGMA user_version=2; COMMIT")
            .map_err(|error| fail(&error))
    })();
    if result.is_err() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    result
}

fn add_delivery(connection: &Connection) -> Result<(), ExportSpoolError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
            ALTER TABLE binding ADD COLUMN next_delivery_id INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE binding ADD COLUMN last_confirmed_batch TEXT;
            ALTER TABLE binding ADD COLUMN confirmed_records INTEGER NOT NULL DEFAULT 0;
            CREATE TABLE delivery (
                id INTEGER PRIMARY KEY CHECK(id=1),
                batch_id TEXT NOT NULL
            );
            CREATE TABLE delivery_items (
                ordinal INTEGER PRIMARY KEY,
                row_id INTEGER NOT NULL UNIQUE REFERENCES pending(row_id) ON DELETE RESTRICT
            );
            PRAGMA user_version=3;
            COMMIT",
        )
        .map_err(|error| {
            if !connection.is_autocommit() {
                let _ = connection.execute_batch("ROLLBACK");
            }
            fail(&error)
        })
}

fn migrate(
    connection: &Connection,
    budget: &RuntimeResourceBudget,
) -> Result<(), ExportSpoolError> {
    let _reservation = budget
        .try_reserve(WorkClass::ExporterBatch, 3 * 1024 * 1024)
        .map_err(|_| {
            ExportSpoolError::new(
                ExportSpoolErrorCode::Backpressure,
                "migration memory unavailable",
            )
        })?;
    connection
        .execute_batch("BEGIN IMMEDIATE; ALTER TABLE pending RENAME TO pending_v1;")
        .map_err(|error| fail(&error))?;
    let result = (|| {
        connection
            .execute_batch(PENDING_V2)
            .map_err(|error| fail(&error))?;
        let mut previous = (-1_i64, -1_i64);
        loop {
            let old: Option<(i64, i64, i64)> = connection
                .query_row(
                    "SELECT durability,sequence,length(payload) FROM pending_v1
                 WHERE durability>?1 OR (durability=?1 AND sequence>?2)
                 ORDER BY durability,sequence LIMIT 1",
                    params![previous.0, previous.1],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|error| fail(&error))?;
            let Some((durability, sequence, length)) = old else {
                break;
            };
            let length = usize::try_from(length).map_err(|_| integrity())?;
            if durability != 0 && durability != 1
                || sequence <= 0
                || length > EXPORT_SPOOL_PAGE_BYTES
            {
                return Err(integrity());
            }
            let bytes: Vec<u8> = connection
                .query_row(
                    "SELECT payload FROM pending_v1 WHERE durability=?1 AND sequence=?2",
                    params![durability, sequence],
                    |row| row.get(0),
                )
                .map_err(|error| fail(&error))?;
            if bytes.len() != length {
                return Err(integrity());
            }
            let decoded: LegacyRecord<'_> =
                serde_json::from_slice(&bytes).map_err(|_| integrity())?;
            let fields = [
                decoded.record_id.as_bytes(),
                decoded.committed_at.get().as_bytes(),
                decoded.record.get().as_bytes(),
            ];
            connection
                .execute(
                    "DELETE FROM pending_v1 WHERE durability=?1 AND sequence=?2",
                    params![durability, sequence],
                )
                .map_err(|error| fail(&error))?;
            connection
                .execute(
                    "INSERT INTO pending(durability,sequence,record_id,committed_at,payload)
                 VALUES(?1,?2,zeroblob(?3),zeroblob(?4),zeroblob(?5))",
                    params![
                        durability,
                        sequence,
                        i64::try_from(fields[0].len()).map_err(|_| integrity())?,
                        i64::try_from(fields[1].len()).map_err(|_| integrity())?,
                        i64::try_from(fields[2].len()).map_err(|_| integrity())?
                    ],
                )
                .map_err(|error| fail(&error))?;
            let row_id = connection.last_insert_rowid();
            for (column, field) in ["record_id", "committed_at", "payload"]
                .into_iter()
                .zip(fields)
            {
                if !field.is_empty() {
                    let mut blob = connection
                        .blob_open(MAIN_DB, "pending", column, row_id, false)
                        .map_err(|error| fail(&error))?;
                    for (index, chunk) in field.chunks(64 * 1024).enumerate() {
                        blob.write_at(chunk, index * 64 * 1024)
                            .map_err(|error| fail(&error))?;
                    }
                }
            }
            previous = (durability, sequence);
        }
        connection
            .execute_batch("DROP TABLE pending_v1; PRAGMA user_version=2; COMMIT")
            .map_err(|error| fail(&error))
    })();
    if result.is_err() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    result
}
