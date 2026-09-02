use rusqlite::{Connection, OptionalExtension};
use uob_application::StorageError;

use crate::configuration::unavailable;

pub(crate) fn migrate(connection: &Connection) -> Result<(), StorageError> {
    create_schema(connection)?;
    upgrade_columns(connection)?;
    connection
        .execute_batch("PRAGMA user_version = 5;")
        .map_err(unavailable)
}

fn create_schema(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS station_snapshots (\n\
                 station_key TEXT PRIMARY KEY, payload TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS authorization_changes (\n\
                 reference TEXT PRIMARY KEY, resource TEXT NOT NULL, state INTEGER NOT NULL,\n\
                 revision INTEGER NOT NULL CHECK (revision >= 0), changed_at TEXT NOT NULL,\n\
                 expires_at TEXT\n\
             );\n\
             CREATE TABLE IF NOT EXISTS commands (\n\
                 request_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,\n\
                 admitted_at INTEGER NOT NULL, retain_until INTEGER NOT NULL,\n\
                 unresolved INTEGER NOT NULL DEFAULT 1 CHECK (unresolved IN (0, 1)),\n\
                 payload TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS command_results (\n\
                 request_id TEXT PRIMARY KEY, payload TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS journal_events (\n\
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE,\n\
                 resource TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence >= 0),\n\
                 payload TEXT NOT NULL, retain_until INTEGER, UNIQUE(resource, sequence)\n\
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
                 retry_at TEXT, retain_until INTEGER\n\
             );\n\
             CREATE INDEX IF NOT EXISTS target_delivery_attempts_delivery_row\n\
                 ON target_delivery_attempts(delivery_id, row_id);\n\
             CREATE TABLE IF NOT EXISTS committed_records (\n\
                 row_id INTEGER PRIMARY KEY AUTOINCREMENT, record_id TEXT NOT NULL UNIQUE,\n\
                 durability INTEGER NOT NULL, committed_at TEXT NOT NULL, payload TEXT NOT NULL,\n\
                 retain_until INTEGER\n\
             );\n\
             CREATE TABLE IF NOT EXISTS storage_retention_stats (\n\
                 category TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0\n\
                     CHECK (count >= 0)\n\
             );\n\
             PRAGMA user_version = 5;",
        )
        .map_err(unavailable)
}

fn upgrade_columns(connection: &Connection) -> Result<(), StorageError> {
    add_column_if_missing(
        connection,
        "authorization_changes",
        "expires_at",
        "ALTER TABLE authorization_changes ADD COLUMN expires_at TEXT",
    )?;
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
    )?;
    add_column_if_missing(
        connection,
        "commands",
        "fingerprint",
        "ALTER TABLE commands ADD COLUMN fingerprint TEXT",
    )?;
    add_column_if_missing(
        connection,
        "commands",
        "admitted_at",
        "ALTER TABLE commands ADD COLUMN admitted_at INTEGER",
    )?;
    add_column_if_missing(
        connection,
        "commands",
        "retain_until",
        "ALTER TABLE commands ADD COLUMN retain_until INTEGER",
    )?;
    add_column_if_missing(
        connection,
        "commands",
        "unresolved",
        "ALTER TABLE commands ADD COLUMN unresolved INTEGER NOT NULL DEFAULT 1 CHECK (unresolved IN (0, 1))",
    )?;
    add_column_if_missing(
        connection,
        "journal_events",
        "retain_until",
        "ALTER TABLE journal_events ADD COLUMN retain_until INTEGER",
    )?;
    add_column_if_missing(
        connection,
        "target_delivery_attempts",
        "retain_until",
        "ALTER TABLE target_delivery_attempts ADD COLUMN retain_until INTEGER",
    )?;
    add_column_if_missing(
        connection,
        "committed_records",
        "retain_until",
        "ALTER TABLE committed_records ADD COLUMN retain_until INTEGER",
    )?;
    Ok(())
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
