use rusqlite::{Connection, OptionalExtension};
use uob_application::{StorageError, StorageErrorCode};

use crate::configuration::unavailable;

pub(crate) fn migrate(connection: &Connection) -> Result<(), StorageError> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(unavailable)?;
    if version > 18 {
        return Err(StorageError::new(
            StorageErrorCode::Unavailable,
            "operational database schema is newer than this release",
        ));
    }
    let transaction = connection.unchecked_transaction().map_err(unavailable)?;
    create_schema(&transaction)?;
    upgrade_columns(&transaction)?;
    crate::charging_profile201::create(&transaction)?;
    crate::reservation16::create(&transaction)?;
    crate::reservation201::create(&transaction)?;
    crate::firmware16::create(&transaction)?;
    crate::target_disposition::create(&transaction)?;
    add_column_if_missing(
        &transaction,
        "command_results",
        "report_pending",
        "ALTER TABLE command_results ADD COLUMN report_pending INTEGER NOT NULL DEFAULT 0 CHECK(report_pending IN (0,1))",
    )?;
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS device_report_pending ON command_results(request_id) WHERE report_pending = 1;
         CREATE TABLE IF NOT EXISTS device_report_staging(
             request_id TEXT PRIMARY KEY REFERENCES command_results(request_id) ON DELETE CASCADE,
             payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 1048576),
             accepted_fragments INTEGER NOT NULL CHECK(accepted_fragments BETWEEN 0 AND 256),
             accepted_items INTEGER NOT NULL CHECK(accepted_items BETWEEN 0 AND 4096),
             accepted_bytes INTEGER NOT NULL CHECK(accepted_bytes BETWEEN 0 AND 1048576)
         );",
    ).map_err(unavailable)?;
    transaction
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS commands_station_history
             ON commands(json_extract(payload, '$.resource.bridge_id'),
                         json_extract(payload, '$.resource.station_id'),
                         admitted_at DESC, request_id DESC);",
        )
        .map_err(unavailable)?;
    if version < 10 {
        transaction
            .execute_batch("DROP INDEX IF EXISTS trigger_pending_results;")
            .map_err(unavailable)?;
    }
    transaction
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS trigger_pending_results
         ON command_results(json_extract(payload,'$.trigger_observation.status'), request_id)
         WHERE trigger_reconcile_active = 1;
         CREATE INDEX IF NOT EXISTS trigger_station_events
         ON journal_events(resource, json_extract(payload,'$.payload.class'),
            json_extract(payload,'$.payload.connector_id'),
            julianday(json_extract(payload,'$.observed_at')))
         WHERE json_valid(payload) AND json_type(payload,'$.payload.class') IS NOT NULL;",
        )
        .map_err(unavailable)?;
    if version < 11 {
        transaction
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS trigger201_pending_results
                 ON command_results(json_extract(payload,'$.trigger_observation_201.status'), request_id)
                 WHERE trigger_reconcile_active = 1;
                 CREATE INDEX IF NOT EXISTS trigger201_station_events
                 ON journal_events(resource, json_extract(payload,'$.payload.trigger_class_201'),
                    json_extract(payload,'$.payload.target.kind'),
                    json_extract(payload,'$.payload.target.id'),
                    json_extract(payload,'$.payload.target.connector_id'),
                    julianday(json_extract(payload,'$.observed_at')))
                 WHERE json_valid(payload)
                   AND json_type(payload,'$.payload.trigger_class_201') IS NOT NULL;",
            )
            .map_err(unavailable)?;
    }
    upgrade_committed_source_streams(&transaction, version)?;
    // Snapshots are validated on every open. Journal rewrites are versioned so a
    // current database does not re-decode its entire retained history on restart.
    normalize_station_keys(&transaction, version < 8)?;
    if version < 9 {
        normalize_event_stream_keys(&transaction)?;
    }
    transaction.execute(
        "UPDATE event_sequence_counter SET value = MAX(value,
         COALESCE((SELECT MAX(sequence) FROM journal_events), 0),
         COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'journal_events'), 0)) WHERE id = 1",
        [],
    ).map_err(unavailable)?;
    transaction
        .execute_batch("PRAGMA user_version = 18;")
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}
fn upgrade_committed_source_streams(
    connection: &Connection,
    version: i64,
) -> Result<(), StorageError> {
    if version < 12 {
        add_column_if_missing(
            connection,
            "committed_records",
            "source_sequence",
            "ALTER TABLE committed_records ADD COLUMN source_sequence INTEGER CHECK (source_sequence > 0)",
        )?;
        connection.execute_batch(
            "WITH numbered AS (
                 SELECT row_id, ROW_NUMBER() OVER (PARTITION BY durability ORDER BY row_id) AS position
                 FROM committed_records
             )
             UPDATE committed_records SET source_sequence =
                 (SELECT position FROM numbered WHERE numbered.row_id = committed_records.row_id);
             UPDATE committed_source_streams SET high_water =
                 COALESCE((SELECT MAX(source_sequence) FROM committed_records
                           WHERE durability = committed_source_streams.durability), 0);",
        ).map_err(unavailable)?;
        connection
            .execute(
                "UPDATE committed_source_identity
             SET legacy_baseline_incomplete =
                 (?1 OR EXISTS(SELECT 1 FROM committed_records))",
                [i64::from(version > 0)],
            )
            .map_err(unavailable)?;
    }
    connection
        .execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS committed_records_source_position
         ON committed_records(durability, source_sequence);",
        )
        .map_err(unavailable)
}

fn normalize_station_keys(connection: &Connection, upgrade: bool) -> Result<(), StorageError> {
    let mut last_row = 0;
    loop {
        let rows = {
            let mut statement = connection
                .prepare(
                    "SELECT rowid, station_key, payload FROM station_snapshots
                     WHERE rowid > ?1 ORDER BY rowid LIMIT 128",
                )
                .map_err(unavailable)?;
            statement
                .query_map([last_row], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(unavailable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unavailable)?
        };
        if rows.is_empty() {
            break;
        }
        for (row_id, old_key, payload) in rows {
            let snapshot = crate::codec::decode_snapshot(&payload)?;
            let stored_key = crate::codec::resource_key(&snapshot.station)?;
            let canonical = crate::snapshots::station_key(&snapshot.station).map_err(|_| {
                StorageError::new(
                    StorageErrorCode::IntegrityFailure,
                    "persisted snapshot station key is invalid",
                )
            })?;
            if old_key != stored_key && old_key != canonical {
                return Err(StorageError::new(
                    StorageErrorCode::IntegrityFailure,
                    "persisted snapshot station key disagrees with payload",
                ));
            }
            if old_key != canonical {
                if !upgrade {
                    return Err(StorageError::new(
                        StorageErrorCode::IntegrityFailure,
                        "persisted snapshot station key is not canonical",
                    ));
                }
                connection
                    .execute(
                        "UPDATE station_snapshots SET station_key = ?1 WHERE rowid = ?2",
                        rusqlite::params![canonical, row_id],
                    )
                    .map_err(unavailable)?;
            }
            last_row = row_id;
        }
    }
    Ok(())
}

fn normalize_event_stream_keys(connection: &Connection) -> Result<(), StorageError> {
    let mut last_row = 0;
    loop {
        let rows = {
            let mut statement = connection
                .prepare(
                    "SELECT row_id, resource, payload FROM journal_events
                     WHERE row_id > ?1 ORDER BY row_id LIMIT 128",
                )
                .map_err(unavailable)?;
            statement
                .query_map([last_row], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(unavailable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unavailable)?
        };
        if rows.is_empty() {
            break;
        }
        for (row_id, old_key, payload) in rows {
            let event = crate::codec::decode_event::<serde_json::Value>(&payload)?;
            let stored_key = crate::codec::resource_key(&event.resource)?;
            let canonical = crate::snapshots::event_stream_key(&event.resource).map_err(|_| {
                StorageError::new(
                    StorageErrorCode::IntegrityFailure,
                    "persisted event resource is invalid",
                )
            })?;
            if old_key != stored_key && old_key != canonical {
                return Err(StorageError::new(
                    StorageErrorCode::IntegrityFailure,
                    "persisted event resource disagrees with payload",
                ));
            }
            if old_key != canonical {
                connection
                    .execute(
                        "UPDATE journal_events SET resource = ?1 WHERE row_id = ?2",
                        rusqlite::params![canonical, row_id],
                    )
                    .map_err(unavailable)?;
            }
            last_row = row_id;
        }
    }
    Ok(())
}

fn create_schema(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS release_jobs (id TEXT PRIMARY KEY, kind TEXT NOT NULL);\n\
             CREATE TABLE IF NOT EXISTS transaction_id_counter (id INTEGER PRIMARY KEY CHECK (id = 1), value INTEGER NOT NULL);\n\
             INSERT OR IGNORE INTO transaction_id_counter VALUES (1, 0);\n\
             CREATE TABLE IF NOT EXISTS event_sequence_counter (id INTEGER PRIMARY KEY CHECK (id = 1), value INTEGER NOT NULL CHECK (value >= 0));\n\
             INSERT OR IGNORE INTO event_sequence_counter VALUES (1, 0);\n\
             CREATE TABLE IF NOT EXISTS station_snapshots (\n\
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
             CREATE TABLE IF NOT EXISTS remote_start_counter (id INTEGER PRIMARY KEY CHECK (id = 1), value INTEGER NOT NULL);\n\
             INSERT OR IGNORE INTO remote_start_counter VALUES (1, 0);\n\
             CREATE TABLE IF NOT EXISTS remote_control_evidence (request_id TEXT PRIMARY KEY REFERENCES commands(request_id) ON DELETE CASCADE, payload TEXT NOT NULL);\n\
             CREATE TABLE IF NOT EXISTS command_results (\n\
                 request_id TEXT PRIMARY KEY, payload TEXT NOT NULL,\n\
                 trigger_reconcile_active INTEGER NOT NULL DEFAULT 1 CHECK (trigger_reconcile_active IN (0, 1))\n\
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
                 retain_until INTEGER, source_sequence INTEGER CHECK (source_sequence > 0)
             );\n\
             CREATE TABLE IF NOT EXISTS committed_source_identity (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 generation TEXT NOT NULL, legacy_baseline_incomplete INTEGER NOT NULL
                     CHECK (legacy_baseline_incomplete IN (0, 1))
             );\n\
             INSERT OR IGNORE INTO committed_source_identity VALUES (1, lower(hex(randomblob(16))), 0);\n\
             CREATE TABLE IF NOT EXISTS committed_source_streams (
                 durability INTEGER PRIMARY KEY CHECK (durability IN (0, 1)),
                 high_water INTEGER NOT NULL DEFAULT 0 CHECK (high_water >= 0),
                 expired_prefix INTEGER NOT NULL DEFAULT 0 CHECK (expired_prefix >= 0)
             );\n\
             INSERT OR IGNORE INTO committed_source_streams(durability) VALUES (0), (1);\n\
             CREATE TABLE IF NOT EXISTS storage_retention_stats (\n\
                 category TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0\n\
                     CHECK (count >= 0)\n\
             );",
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
    add_column_if_missing(
        connection,
        "command_results",
        "trigger_reconcile_active",
        "ALTER TABLE command_results ADD COLUMN trigger_reconcile_active INTEGER NOT NULL DEFAULT 1 CHECK (trigger_reconcile_active IN (0, 1))",
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
