//! OCPP 1.6 security-event dedup read. A redelivered `SecurityEventNotification` is recognised
//! from the journal row the first delivery committed, through the partial expression index
//! `security16_station_events`, which holds the whole key, so the read examines one index entry
//! however many other events the station has.
use crate::{SqliteOperationalStore, configuration::unavailable, snapshots, worker::Request};
use rusqlite::{Connection, Statement, params};
use serde::de::DeserializeOwned;
use uob_application::{
    SecurityEventKey16, SecurityEventStore16, StorageError, StorageErrorCode, StorageFuture,
};

/// Exact-key existence lookup. It repeats both guards of the index and binds every key part, so
/// SQLite answers it from one index entry and never loads a journal row. `IS` makes an absent
/// `techInfo` (`NULL`, the field is not serialized) equal only to an absent one, while a present
/// value, including the empty string, equals only the identical text.
pub(crate) const RECORDED: &str =
    "SELECT EXISTS(SELECT 1 FROM journal_events INDEXED BY security16_station_events
     WHERE resource = ?1
       AND json_extract(payload,'$.payload.security_event_16.timestamp') = ?2
       AND json_extract(payload,'$.payload.security_event_16.event_type') = ?3
       AND json_extract(payload,'$.payload.security_event_16.tech_info') IS ?4
       AND json_valid(payload)
       AND json_type(payload,'$.payload.security_event_16') IS NOT NULL)";

/// A key whose station is already the canonical journal stream key and whose timestamp is the
/// exact text the journal serializes.
pub(crate) struct Query {
    station: String,
    timestamp: String,
    event_type: String,
    tech_info: Option<String>,
}

impl Query {
    fn new(key: SecurityEventKey16) -> Result<Self, StorageError> {
        let Ok(serde_json::Value::String(timestamp)) = serde_json::to_value(key.timestamp) else {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "security event timestamp could not be encoded",
            ));
        };

        Ok(Self {
            station: snapshots::station_key(&key.station)?,
            timestamp,
            event_type: key.event_type,
            tech_info: key.tech_info,
        })
    }
}

/// Whether the station's stream holds an event with this timestamp, type and `techInfo`. The
/// values are compared as `json_extract` text (JSON escapes already decoded, `BINARY`
/// collation), so escaping cannot matter and the cost does not depend on how many other events
/// share the station and timestamp.
pub(crate) fn recorded(connection: &Connection, query: &Query) -> Result<bool, StorageError> {
    let mut statement = connection.prepare(RECORDED).map_err(unavailable)?;

    run(&mut statement, query).map_err(unavailable)
}

fn run(statement: &mut Statement<'_>, query: &Query) -> rusqlite::Result<bool> {
    statement.query_row(
        params![
            query.station,
            query.timestamp,
            query.event_type,
            query.tech_info
        ],
        |row| row.get(0),
    )
}

impl<C, E, D, R> SecurityEventStore16 for SqliteOperationalStore<C, E, D, R>
where
    C: DeserializeOwned + serde::Serialize + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn security_event_recorded_16(&self, key: SecurityEventKey16) -> StorageFuture<'_, bool> {
        let query = match Query::new(key) {
            Ok(query) => query,
            Err(error) => return Box::pin(async move { Err(error) }),
        };

        self.request(|reply| Request::SecurityEvent16(query, reply))
    }
}

#[cfg(test)]
mod tests;
