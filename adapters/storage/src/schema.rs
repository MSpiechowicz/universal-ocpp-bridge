use rusqlite::{Connection, OptionalExtension};
use uob_application::StorageError;

use crate::configuration::unavailable;

pub(crate) fn migrate(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS station_snapshots (\n\
                 station_key TEXT PRIMARY KEY, payload TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS authorization_changes (\n\
                 reference TEXT PRIMARY KEY, resource TEXT NOT NULL, state INTEGER NOT NULL,\n\
                 revision INTEGER NOT NULL CHECK (revision >= 0), changed_at TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS commands (\n\
                 request_id TEXT PRIMARY KEY, payload TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS command_results (\n\
                 request_id TEXT PRIMARY KEY, payload TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS journal_events (\n\
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE,\n\
                 resource TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence >= 0),\n\
                 payload TEXT NOT NULL, UNIQUE(resource, sequence)\n\
             );\n\
             CREATE INDEX IF NOT EXISTS journal_events_resource_row\n\
                 ON journal_events(resource, row_id);\n\
             CREATE TABLE IF NOT EXISTS target_deliveries (\n\
                 target_instance_id TEXT NOT NULL, target_revision INTEGER NOT NULL\n\
                     CHECK (target_revision >= 0),\n\
                 event_id TEXT NOT NULL, delivery_id TEXT NOT NULL, ordering_key TEXT NOT NULL,\n\
                 deadline TEXT NOT NULL, durability INTEGER NOT NULL, payload TEXT NOT NULL,\n\
                 attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),\n\
                 next_attempt_at TEXT,\n\
                 PRIMARY KEY(target_instance_id, target_revision, event_id),\n\
                 UNIQUE(delivery_id)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS target_delivery_attempts (\n\
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT, delivery_id TEXT NOT NULL,\n\
                 outcome TEXT NOT NULL, reported_at TEXT NOT NULL, resolution INTEGER NOT NULL,\n\
                 retry_at TEXT\n\
             );\n\
             CREATE INDEX IF NOT EXISTS target_delivery_attempts_delivery_row\n\
                 ON target_delivery_attempts(delivery_id, row_id);\n\
             CREATE TABLE IF NOT EXISTS committed_records (\n\
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT, record_id TEXT NOT NULL UNIQUE,\n\
                 durability INTEGER NOT NULL, committed_at TEXT NOT NULL, payload TEXT NOT NULL\n\
             );\n\
             PRAGMA user_version = 2;",
        )
        .map_err(unavailable)?;
    add_column_if_missing(
        connection,
        "target_deliveries",
        "attempt_count",
        "ALTER TABLE target_deliveries ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0)",
    )?;
    add_column_if_missing(
        connection,
        "target_deliveries",
        "next_attempt_at",
        "ALTER TABLE target_deliveries ADD COLUMN next_attempt_at TEXT",
    )
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    statement: &str,
) -> Result<(), StorageError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
            [table, column],
            |_| Ok(()),
        )
        .optional()
        .map_err(unavailable)?
        .is_some();
    if !exists {
        connection.execute_batch(statement).map_err(unavailable)?;
    }
    Ok(())
}
