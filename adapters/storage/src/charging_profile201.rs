//! Bounded native201 ownership on the ordinary SQLite transaction/worker boundary.
use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    ChargingProfileStore201, ProfileMutation201, ProfileOwnership201, StorageError,
    StorageErrorCode, StorageFuture, baseline_purpose, purpose_index,
};
use uob_contracts::{
    ChargingProfileResult201, ClearChargingProfileStatus201, CommandLifecycle, CommandResult,
    ResourceRef, StationSnapshot, TransactionState,
};
mod fence;
mod metadata;
mod ownership;
mod schema;
pub(crate) use fence::recover;
pub(crate) use ownership::read;
pub(crate) use ownership::reserve;
pub(crate) use schema::create;

impl<C, E, D, R> ChargingProfileStore201 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn charging_profile_ownership(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, ProfileOwnership201> {
        self.request(|reply| Request::ChargingProfileOwnership(station, reply))
    }
    fn interrupt_charging_profile_mutations(&self) -> StorageFuture<'_, ()> {
        self.request(Request::InterruptChargingProfiles)
    }
}

/// Called after merging terminal result immutability, before persisting result, in one transaction.
pub(crate) fn finish(
    transaction: &Transaction<'_>,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let request = result.return_route.request_id.as_str();
    let reservation = transaction
        .query_row(
            "SELECT station,payload FROM charging_profile201_mutations WHERE request_id=?1",
            [request],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((station, payload)) = reservation else {
        return Ok(());
    };
    let reservation = metadata::decode_mutation(&payload)?;
    validate_acknowledgement(&reservation.mutation, result)?;
    match &result.lifecycle {
        CommandLifecycle::Admitted => return Ok(()),
        CommandLifecycle::Dispatched => {
            transaction
                .execute(
                    "UPDATE charging_profile201_mutations SET started=1 WHERE request_id=?1",
                    [request],
                )
                .map_err(unavailable)?;
            return Ok(());
        }
        CommandLifecycle::TransmissionUncertain { .. } => {
            transaction.execute("UPDATE charging_profile201_footprints SET state=2 WHERE station=?1 AND owner=?2",
                params![station, request]).map_err(unavailable)?;
        }
        CommandLifecycle::ProtocolResponse { accepted, .. } => match &reservation.mutation {
            ProfileMutation201::Set { footprint, .. } => {
                if *accepted {
                    finish_set(transaction, &station, request, footprint.id)?;
                } else {
                    release_candidate(transaction, &station, request)?;
                }
            }
            ProfileMutation201::Clear(clear) => finish_clear(
                transaction,
                &station,
                request,
                clear,
                *accepted,
                result.charging_profile_201.as_ref(),
            )?,
        },
        CommandLifecycle::Rejected { .. } => release_candidate(transaction, &station, request)?,
    }
    transaction
        .execute(
            "DELETE FROM charging_profile201_mutations WHERE request_id=?1",
            [request],
        )
        .map_err(unavailable)?;
    Ok(())
}

fn validate_acknowledgement(
    mutation: &ProfileMutation201,
    result: &CommandResult,
) -> Result<(), StorageError> {
    if let Some(evidence) = &result.charging_profile_201 {
        let matches = match (mutation, evidence) {
            (
                ProfileMutation201::Set {
                    footprint,
                    full_native: true,
                },
                ChargingProfileResult201::SetChargingProfile { request, .. },
            ) => {
                let profile = &request.charging_profile;
                footprint.id == profile.id
                    && footprint.evse_id == request.evse_id
                    && footprint.purpose == profile.charging_profile_purpose
                    && footprint.stack_level == profile.stack_level
                    && footprint.transaction_id == profile.transaction_id
                    && footprint.valid_from == profile.valid_from
                    && footprint.valid_to == profile.valid_to
            }
            (
                ProfileMutation201::Clear(clear),
                ChargingProfileResult201::ClearChargingProfile { request, .. },
            ) => clear == request,
            _ => false,
        };
        if !matches
            || !matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == evidence.accepted())
        {
            return Err(StorageError::new(
                StorageErrorCode::IntegrityFailure,
                "profile acknowledgement changed reserved request",
            ));
        }
    }
    if matches!(
        result.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ) && !matches!(
        mutation,
        ProfileMutation201::Set {
            full_native: false,
            ..
        }
    ) && result.charging_profile_201.is_none()
    {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "native profile acceptance lacks acknowledgement",
        ));
    }
    Ok(())
}

fn finish_set(
    transaction: &Transaction<'_>,
    station: &str,
    request: &str,
    profile_id: i32,
) -> Result<(), StorageError> {
    // Never insert: an actual transaction-end write may already have retired the candidate.
    transaction.execute("DELETE FROM charging_profile201_footprints WHERE station=?1 AND profile_id=?2 AND owner<>?3",
        params![station, profile_id, request]).map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE charging_profile201_footprints SET state=1 WHERE station=?1 AND owner=?2",
            params![station, request],
        )
        .map_err(unavailable)?;
    Ok(())
}

fn finish_clear(
    transaction: &Transaction<'_>,
    station: &str,
    request: &str,
    clear: &uob_contracts::ClearChargingProfileRequest201,
    accepted: bool,
    evidence: Option<&ChargingProfileResult201>,
) -> Result<(), StorageError> {
    let purpose_unknown = matches!(evidence, Some(ChargingProfileResult201::ClearChargingProfile {
        status: ClearChargingProfileStatus201::Unknown, request, .. })
        if request == clear && baseline_purpose(clear).is_some());
    if accepted || purpose_unknown {
        clear_known(transaction, station, clear, true)
    } else {
        release_candidate(transaction, station, request)
    }
}
fn release_candidate(
    transaction: &Transaction<'_>,
    station: &str,
    request: &str,
) -> Result<(), StorageError> {
    transaction
        .execute(
            "DELETE FROM charging_profile201_footprints WHERE station=?1 AND owner=?2 AND state=0",
            params![station, request],
        )
        .map_err(unavailable)
        .map(|_| ())
}
fn clear_known(
    transaction: &Transaction<'_>,
    station: &str,
    clear: &uob_contracts::ClearChargingProfileRequest201,
    establish: bool,
) -> Result<(), StorageError> {
    for (rowid, footprint) in ownership::footprints(transaction, station)? {
        if footprint.matches_clear(clear) {
            transaction
                .execute(
                    "DELETE FROM charging_profile201_footprints WHERE rowid=?1",
                    [rowid],
                )
                .map_err(unavailable)?;
        }
    }
    if establish && let Some(purpose) = baseline_purpose(clear) {
        let mut baseline = [false; 3];
        baseline[purpose_index(purpose)] = true;
        transaction.execute("INSERT INTO charging_profile201_baseline(station,max_known,default_known,tx_known) VALUES(?1,?2,?3,?4)
            ON CONFLICT(station) DO UPDATE SET max_known=MAX(max_known,excluded.max_known),
            default_known=MAX(default_known,excluded.default_known),tx_known=MAX(tx_known,excluded.tx_known)",
            params![station, baseline[0], baseline[1], baseline[2]]).map_err(unavailable)?;
    }
    Ok(())
}

/// Retire only an explicit actual Ended observation, never disconnect/missing state/history pruning.
pub(crate) fn retire_ended(
    transaction: &Transaction<'_>,
    snapshot: &StationSnapshot,
) -> Result<(), StorageError> {
    let ended = snapshot
        .transactions
        .iter()
        .filter(|tx| tx.state == TransactionState::Ended)
        .filter_map(|tx| tx.protocol_state.as_ref())
        .filter(|state| state.protocol == uob_contracts::ProtocolEdition::Ocpp201)
        .map(|state| state.native_transaction_id.as_str())
        .collect::<Vec<_>>();
    if ended.is_empty() {
        return Ok(());
    }
    let station = ownership::station_key(&snapshot.station)?;
    for (rowid, footprint) in ownership::footprints(transaction, &station)? {
        if footprint
            .transaction_id
            .as_deref()
            .is_some_and(|id| ended.contains(&id))
        {
            transaction
                .execute(
                    "DELETE FROM charging_profile201_footprints WHERE rowid=?1",
                    [rowid],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}
