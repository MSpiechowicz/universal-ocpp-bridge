use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use uob_application::{Page, SnapshotCursor, StorageError, StorageErrorCode};
use uob_contracts::{
    BridgeId, CanonicalResource, NativeProtocolReference, ResourceRef, StationId, StationSnapshot,
};

use crate::{codec, configuration::unavailable};

/// Snapshot keys are bridge-qualified logical station identities, not native controller addresses.
/// Retain the original native reference only in the snapshot payload.
pub(crate) fn station_key(value: &ResourceRef) -> Result<String, StorageError> {
    if value.resource.is_some()
        || !matches!(
            value.native_protocol_reference,
            None | Some(
                NativeProtocolReference::Ocpp16 { connector_id: 0 }
                    | NativeProtocolReference::Ocpp201 {
                        evse_id: 0,
                        connector_id: None,
                    },
            )
        )
    {
        return Err(StorageError::new(
            StorageErrorCode::InvalidRequest,
            "snapshot scope must contain station-only canonical references",
        ));
    }
    codec::resource_key(&ResourceRef {
        bridge_id: value.bridge_id.clone(),
        station_id: value.station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    })
}

/// Retained streams use canonical resource identities; native controller addresses
/// remain in event envelopes but never split a station or child stream.
pub(crate) fn event_stream_key(value: &ResourceRef) -> Result<String, StorageError> {
    if let Some(resource) = value.resource.as_ref() {
        #[derive(Serialize)]
        struct CanonicalChildKey<'a> {
            bridge_id: &'a BridgeId,
            station_id: &'a StationId,
            resource: &'a CanonicalResource,
        }

        serde_json::to_string(&CanonicalChildKey {
            bridge_id: &value.bridge_id,
            station_id: &value.station_id,
            resource,
        })
        .map_err(|_| {
            StorageError::new(
                StorageErrorCode::InvalidRequest,
                "record serialization failed",
            )
        })
    } else {
        station_key(value)
    }
}

pub(crate) fn read(
    connection: &Connection,
    after: Option<String>,
    limit: usize,
) -> Result<Page<StationSnapshot, SnapshotCursor>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT station_key, payload FROM station_snapshots
             WHERE station_key > ?1 ORDER BY station_key LIMIT ?2",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![after.unwrap_or_default(), page_bound(limit)?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(unavailable)?;
    page(
        rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?,
        limit,
    )
}

pub(crate) fn exact(
    connection: &Connection,
    key: &str,
) -> Result<Option<StationSnapshot>, StorageError> {
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM station_snapshots WHERE station_key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    payload.as_deref().map(codec::decode_snapshot).transpose()
}

pub(crate) fn scoped(
    connection: &Connection,
    keys: &str,
    after: Option<String>,
    limit: usize,
) -> Result<Page<StationSnapshot, SnapshotCursor>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT station_key, payload FROM station_snapshots
             WHERE station_key IN (SELECT value FROM json_each(?1))
               AND station_key > ?2
             ORDER BY station_key LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![keys, after.unwrap_or_default(), page_bound(limit)?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(unavailable)?;
    page(
        rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?,
        limit,
    )
}

fn page_bound(limit: usize) -> Result<i64, StorageError> {
    i64::try_from(limit.saturating_add(1)).map_err(|_| unavailable(rusqlite::Error::InvalidQuery))
}

fn page(
    mut values: Vec<(String, String)>,
    limit: usize,
) -> Result<Page<StationSnapshot, SnapshotCursor>, StorageError> {
    let has_more = values.len() > limit;
    values.truncate(limit);
    let next_cursor = has_more
        .then(|| {
            values
                .last()
                .map(|value| SnapshotCursor::new(value.0.clone()))
        })
        .flatten()
        .transpose()?;
    let items = values
        .into_iter()
        .map(|(_, payload)| codec::decode_snapshot(&payload))
        .collect::<Result<_, _>>()?;
    Ok(Page { items, next_cursor })
}
