use super::metadata::{decode_footprint, encode_footprint, encode_mutation};
use crate::{codec, configuration::unavailable};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use uob_application::{
    MAX_PROFILE_FOOTPRINTS_201, ProfileFootprint201, ProfileMutation201, ProfileOwnership201,
    ProfileReservation201, StorageError, StorageErrorCode,
};
use uob_contracts::ResourceRef;

pub(super) fn station_key(station: &ResourceRef) -> Result<String, StorageError> {
    if station.resource.is_some() || station.native_protocol_reference.is_some() {
        return Err(StorageError::new(
            StorageErrorCode::InvalidRequest,
            "profile ledger requires station scope",
        ));
    }
    crate::snapshots::station_key(station)
}

pub(super) fn footprints(
    connection: &Connection,
    station: &str,
) -> Result<Vec<(i64, ProfileFootprint201)>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT rowid, payload FROM charging_profile201_footprints WHERE station=?1 ORDER BY rowid LIMIT 129"
    ).map_err(unavailable)?;
    let rows = statement
        .query_map([station], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    if rows.len() > MAX_PROFILE_FOOTPRINTS_201 {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "profile ownership exceeds bound",
        ));
    }
    rows.into_iter()
        .map(|(rowid, payload)| Ok((rowid, decode_footprint(&payload)?)))
        .collect()
}

pub(crate) fn read(
    connection: &Connection,
    station: &ResourceRef,
) -> Result<ProfileOwnership201, StorageError> {
    let key = station_key(station)?;
    let baseline = baseline(connection, &key)?;
    let footprints = footprints(connection, &key)?
        .into_iter()
        .map(|(_, footprint)| footprint)
        .collect();
    let busy = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM charging_profile201_mutations WHERE station=?1)",
            [&key],
            |row| row.get::<_, bool>(0),
        )
        .map_err(unavailable)?;
    Ok(ProfileOwnership201 {
        baseline,
        footprints,
        busy,
    })
}
fn baseline(connection: &Connection, station: &str) -> Result<[bool; 3], StorageError> {
    connection.query_row("SELECT max_known, default_known, tx_known FROM charging_profile201_baseline WHERE station=?1",
        [station], |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?])).optional().map_err(unavailable)
        .map(|value| value.unwrap_or([false; 3]))
}

pub(crate) fn reserve(
    transaction: &Transaction<'_>,
    reservation: &ProfileReservation201,
) -> Result<(), StorageError> {
    let station = station_key(&reservation.station)?;
    let command: uob_contracts::Command<serde_json::Value> = transaction
        .query_row(
            "SELECT payload FROM commands WHERE request_id=?1",
            [reservation.request_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .map_err(unavailable)
        .and_then(|payload| codec::decode_command(&payload))?;
    if command.resource.bridge_id != reservation.station.bridge_id
        || command.resource.station_id != reservation.station.station_id
    {
        return Err(conflict("profile reservation station mismatch"));
    }
    let live = transaction
        .query_row(
            "SELECT rowid, connection FROM charging_profile201_mutations WHERE station=?1",
            [&station],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    if let Some((rowid, connection)) = live {
        if matches!(reservation.mutation, ProfileMutation201::Clear(_))
            && connection != reservation.connection.as_str()
        {
            super::fence::retire(transaction, rowid)?;
        } else {
            return Err(conflict("station profile mutation busy"));
        }
    }
    match &reservation.mutation {
        ProfileMutation201::Set {
            footprint,
            full_native,
        } => {
            if reservation.requires_baseline && baseline(transaction, &station)? != [true; 3] {
                return Err(conflict("explicit charging profile baseline required"));
            }
            let existing = footprints(transaction, &station)?;
            if existing.len() >= MAX_PROFILE_FOOTPRINTS_201 {
                return Err(conflict("charging profile ownership capacity exhausted"));
            }
            // Ordinary charging-limit authority cannot replace a privileged policy sharing its ID.
            // Quantities are not ledger metadata, so repeated canonical limits remain compatible.
            if !*full_native
                && existing
                    .iter()
                    .any(|(_, owned)| owned.id == footprint.id && owned != footprint)
            {
                return Err(conflict(
                    "canonical charging profile replacement exceeds authority",
                ));
            }
            // IDs are station-global. An EVSE/connector grant cannot erase another scope's policy
            // through replacement, including any old footprint retained after uncertainty.
            if command.resource.resource.is_some()
                && existing.iter().any(|(_, owned)| {
                    owned.id == footprint.id && owned.evse_id != footprint.evse_id
                })
            {
                return Err(conflict(
                    "charging profile replacement exceeds resource authority",
                ));
            }
            if existing.iter().any(|(_, owned)| owned.conflicts(footprint)) {
                return Err(conflict("charging profile ownership conflict"));
            }
            // Reject a retired transaction using the committed state, including admission/end races.
            if footprint.transaction_id.is_some()
                && !crate::snapshots::exact(transaction, &station)?
                    .as_ref()
                    .is_some_and(|snapshot| transaction_valid(snapshot, footprint))
            {
                return Err(conflict("charging profile transaction unavailable"));
            }
            transaction.execute("INSERT INTO charging_profile201_footprints(station,owner,profile_id,state,payload) VALUES(?1,?2,?3,0,?4)",
                params![station, reservation.request_id.as_str(), footprint.id, encode_footprint(footprint)?]).map_err(unavailable)?;
        }
        ProfileMutation201::Clear(_) => {}
    }
    transaction.execute("INSERT INTO charging_profile201_mutations(request_id,station,connection,payload) VALUES(?1,?2,?3,?4)",
        params![reservation.request_id.as_str(), station, reservation.connection.as_str(), encode_mutation(reservation)?]).map_err(unavailable)?;
    Ok(())
}

fn transaction_valid(
    snapshot: &uob_contracts::StationSnapshot,
    footprint: &ProfileFootprint201,
) -> bool {
    let Some(id) = footprint.transaction_id.as_ref() else {
        return false;
    };
    let mut native_count = 0;
    let mut evse_count = 0;
    let mut matched = false;
    for tx in &snapshot.transactions {
        if tx.state == uob_contracts::TransactionState::Ended {
            continue;
        }
        let Some(state) = tx
            .protocol_state
            .as_ref()
            .filter(|state| state.protocol == uob_contracts::ProtocolEdition::Ocpp201)
        else {
            continue;
        };
        let eligible = matches!(
            tx.state,
            uob_contracts::TransactionState::Pending
                | uob_contracts::TransactionState::Active
                | uob_contracts::TransactionState::Suspended
        ) && tx.resource.bridge_id == snapshot.station.bridge_id
            && tx.resource.station_id == snapshot.station.station_id
            && matches!(tx.resource.native_protocol_reference,
                Some(uob_contracts::NativeProtocolReference::Ocpp201 { evse_id, .. })
                    if i32::try_from(evse_id).ok() == Some(footprint.evse_id));
        if &state.native_transaction_id == id {
            native_count += 1;
            matched |= eligible;
        }
        if eligible {
            evse_count += 1;
        }
        if native_count > 1 || evse_count > 1 {
            return false;
        }
    }
    native_count == 1 && evse_count == 1 && matched
}

pub(super) fn conflict(detail: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::Conflict, detail)
}
