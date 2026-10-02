use crate::configuration::unavailable;
use rusqlite::Connection;
use uob_application::StorageError;

pub(crate) fn create(connection: &Connection) -> Result<(), StorageError> {
    // Deliberately no foreign keys to historical commands: pruning cannot erase ownership.
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS charging_profile201_baseline(
            station TEXT PRIMARY KEY, max_known INTEGER NOT NULL DEFAULT 0,
            default_known INTEGER NOT NULL DEFAULT 0, tx_known INTEGER NOT NULL DEFAULT 0,
            CHECK(max_known IN (0,1) AND default_known IN (0,1) AND tx_known IN (0,1))
        );
        CREATE TABLE IF NOT EXISTS charging_profile201_footprints(
            station TEXT NOT NULL, owner TEXT NOT NULL, profile_id INTEGER NOT NULL,
            state INTEGER NOT NULL CHECK(state IN (0,1,2)),
            payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 4096),
            PRIMARY KEY(station, owner)
        );
        CREATE INDEX IF NOT EXISTS charging_profile201_ids
            ON charging_profile201_footprints(station, profile_id);
        CREATE TABLE IF NOT EXISTS charging_profile201_mutations(
            request_id TEXT PRIMARY KEY, station TEXT NOT NULL UNIQUE,
            connection TEXT NOT NULL, started INTEGER NOT NULL DEFAULT 0 CHECK(started IN (0,1)),
            payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 8192)
        );",
        )
        .map_err(unavailable)
}
