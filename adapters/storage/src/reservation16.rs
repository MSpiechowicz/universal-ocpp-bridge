//! Reservation state remains independent of prunable command history.
use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};
use rusqlite::{Connection, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    MAX_RESERVATION_REVISIONS_16, ReservationMutation16, ReservationMutationKind16,
    ReservationRecord16, ReservationStore16, StorageError, StorageErrorCode, StorageFuture,
};
use uob_contracts::{ReservationState16, ResourceRef, UtcTimestamp};
pub(crate) mod codec_validation;
mod observations;
mod recovery;
mod retention;
mod transitions;
pub(crate) mod validation;
pub(crate) use observations::observe;
pub(crate) use recovery::maintain;
pub(crate) use transitions::finish;

impl<C, E, D, R> ReservationStore16 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn reservations_16(&self, station: ResourceRef) -> StorageFuture<'_, Vec<ReservationRecord16>> {
        self.request(|reply| Request::Reservations16(station, reply))
    }
    fn recover_reservations_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainReservations16(now, true, reply))
    }
    fn expire_reservations_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainReservations16(now, false, reply))
    }
}
pub(crate) fn create(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS reservations16(
        station TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0),
        request_id TEXT NOT NULL UNIQUE, reservation_id INTEGER NOT NULL,
        inflight INTEGER NOT NULL CHECK(inflight IN (0,1)), payload TEXT NOT NULL,
        PRIMARY KEY(station,revision));
        CREATE UNIQUE INDEX IF NOT EXISTS reservations16_owner ON reservations16(station) WHERE inflight=1;
        CREATE INDEX IF NOT EXISTS reservations16_id ON reservations16(station,reservation_id,revision);").map_err(unavailable)
}
pub(crate) fn read(
    connection: &Connection,
    station: &ResourceRef,
) -> Result<Vec<ReservationRecord16>, StorageError> {
    read_key(connection, &crate::snapshots::station_key(station)?)
}
pub(super) fn read_key(
    connection: &Connection,
    station: &str,
) -> Result<Vec<ReservationRecord16>, StorageError> {
    let mut statement = connection
        .prepare("SELECT payload FROM reservations16 WHERE station=?1 ORDER BY revision LIMIT 257")
        .map_err(unavailable)?;
    let values = statement
        .query_map([station], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .map(|row| crate::codec_reservation16::decode(&row.map_err(unavailable)?))
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() > MAX_RESERVATION_REVISIONS_16 {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "reservation history exceeds bounded capacity",
        ));
    }
    Ok(values)
}
pub(super) fn save(
    connection: &Connection,
    record: &ReservationRecord16,
    inflight: Option<bool>,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&record.station)?;
    let revision =
        i64::try_from(record.revision).map_err(|_| conflict("reservation revision exhausted"))?;
    let changed = connection.execute("UPDATE reservations16 SET payload=?3,inflight=COALESCE(?4,inflight) WHERE station=?1 AND revision=?2",
        params![station, revision, crate::codec_reservation16::encode(record)?, inflight]).map_err(unavailable)?;
    if changed != 1 {
        return Err(conflict("reservation workflow owner changed"));
    }
    Ok(())
}
pub(crate) fn reserve(
    transaction: &Transaction<'_>,
    mutation: &ReservationMutation16,
) -> Result<(), StorageError> {
    validation::validate_mutation(transaction, mutation)?;
    let station = crate::snapshots::station_key(&mutation.station)?;
    observe(
        transaction,
        &uob_application::ReservationObservation16 {
            station: mutation.station.clone(),
            observed_at: mutation.admitted_at,
            kind: uob_application::ReservationObservationKind16::Expiry,
        },
    )?;
    retention::prune(transaction, &station, mutation.admitted_at)?;
    let inflight: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reservations16 WHERE station=?1 AND inflight=1)",
            [&station],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if inflight {
        return Err(conflict("station reservation mutation already in flight"));
    }
    let values = read_key(transaction, &station)?;
    if values.len() == MAX_RESERVATION_REVISIONS_16 {
        return Err(conflict("reservation history capacity exhausted"));
    }
    let revision = values.last().map_or(Ok(1), |r| {
        r.revision
            .checked_add(1)
            .ok_or_else(|| conflict("reservation revision exhausted"))
    })?;
    let candidate = match &mutation.mutation {
        ReservationMutationKind16::Reserve(c) => Some(c.clone()),
        ReservationMutationKind16::Cancel => None,
    };
    if candidate
        .as_ref()
        .is_some_and(|c| c.expiry_date <= mutation.admitted_at)
    {
        return Err(conflict("reservation expiry elapsed"));
    }
    let record = ReservationRecord16 {
        station: mutation.station.clone(),
        request_id: mutation.request_id.clone(),
        reservation_id: mutation.reservation_id,
        revision,
        candidate,
        state: ReservationState16::Pending,
        admitted_at: mutation.admitted_at,
        changed_at: mutation.admitted_at,
        source_time: None,
        source_observed_at: None,
        started: false,
        unresolved: true,
        ambiguous: false,
        history_floor: values.last().and_then(|record| record.history_floor),
    };
    transaction.execute("INSERT INTO reservations16(station,revision,request_id,reservation_id,inflight,payload) VALUES(?1,?2,?3,?4,1,?5)",
        params![station, i64::try_from(revision).map_err(|_| conflict("reservation revision exhausted"))?, mutation.request_id.as_str(), mutation.reservation_id, crate::codec_reservation16::encode(&record)?]).map_err(unavailable)?;
    Ok(())
}
pub(super) fn conflict(detail: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::Conflict, detail)
}
