//! Durable OCPP 2.0.1 firmware jobs. Each unresolved job holds one `release_jobs` row, written
//! and removed in the same transaction as the job state, so a release drain sees exactly the
//! physical firmware work that is not yet confirmed finished.
use crate::{
    SqliteOperationalStore, configuration::unavailable, firmware16::register_release_job,
    worker::Request,
};
use rusqlite::{Connection, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    FirmwareJobMutation201, FirmwareJobRecord201, FirmwareObservation201,
    FirmwareObservationKind201, FirmwareStore201, MAX_FIRMWARE_JOBS_201, StorageError,
    StorageErrorCode, StorageFuture,
};
use uob_contracts::{FirmwareJobState201, RequestId, ResourceRef, UtcTimestamp};
mod observations;
mod recovery;
mod transitions;
pub(crate) mod validation;
pub(crate) use observations::observe;
pub(crate) use recovery::maintain;
pub(crate) use transitions::finish;

impl<C, E, D, R> FirmwareStore201 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn firmware_jobs_201(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<FirmwareJobRecord201>> {
        self.request(|reply| Request::FirmwareJobs201(station, reply))
    }
    fn recover_firmware_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainFirmware201(now, true, reply))
    }
    fn expire_firmware_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainFirmware201(now, false, reply))
    }
}

pub(crate) fn create(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS firmware201_jobs(
        station TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0),
        request_id TEXT NOT NULL UNIQUE, native_request_id INTEGER NOT NULL,
        inflight INTEGER NOT NULL CHECK(inflight IN (0,1)), payload TEXT NOT NULL,
        PRIMARY KEY(station,revision), UNIQUE(station,native_request_id));
        CREATE UNIQUE INDEX IF NOT EXISTS firmware201_owner ON firmware201_jobs(station) WHERE inflight=1;",
        )
        .map_err(unavailable)
}

pub(crate) fn read(
    connection: &Connection,
    station: &ResourceRef,
) -> Result<Vec<FirmwareJobRecord201>, StorageError> {
    read_key(connection, &crate::snapshots::station_key(station)?)
}

pub(super) fn read_key(
    connection: &Connection,
    station: &str,
) -> Result<Vec<FirmwareJobRecord201>, StorageError> {
    let mut statement = connection
        .prepare("SELECT payload FROM firmware201_jobs WHERE station=?1 ORDER BY revision LIMIT 65")
        .map_err(unavailable)?;
    let values = statement
        .query_map([station], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .map(|row| crate::codec_firmware201::decode(&row.map_err(unavailable)?))
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() > MAX_FIRMWARE_JOBS_201 {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "firmware job history exceeds bounded capacity",
        ));
    }
    Ok(values)
}

/// Persists one revision and keeps the release-drain inventory in the same transaction.
pub(super) fn save(
    connection: &Connection,
    record: &FirmwareJobRecord201,
    inflight: Option<bool>,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&record.station)?;
    let revision =
        i64::try_from(record.revision).map_err(|_| conflict("firmware revision exhausted"))?;
    let changed = connection
        .execute(
            "UPDATE firmware201_jobs SET payload=?3,inflight=COALESCE(?4,inflight)
             WHERE station=?1 AND revision=?2",
            params![
                station,
                revision,
                crate::codec_firmware201::encode(record)?,
                inflight
            ],
        )
        .map_err(unavailable)?;
    if changed != 1 {
        return Err(conflict("firmware job owner changed"));
    }
    if record.state.resolved() {
        connection
            .execute(
                "DELETE FROM release_jobs WHERE id=?1",
                [release_job_id(&record.request_id)],
            )
            .map_err(unavailable)?;
    }
    Ok(())
}

/// Registers a new job. A station may hold one request in flight; an older accepted update
/// stays unresolved until the station cancels or replaces it (L01.FR.24).
pub(crate) fn reserve(
    transaction: &Transaction<'_>,
    mutation: &FirmwareJobMutation201,
) -> Result<(), StorageError> {
    validation::validate_mutation(transaction, mutation)?;
    let station = crate::snapshots::station_key(&mutation.station)?;
    observe(
        transaction,
        &FirmwareObservation201 {
            station: mutation.station.clone(),
            observed_at: mutation.admitted_at,
            kind: FirmwareObservationKind201::Expiry,
        },
    )?;
    prune(transaction, &station)?;
    let inflight: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM firmware201_jobs WHERE station=?1 AND inflight=1)",
            [&station],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if inflight {
        return Err(conflict("station firmware request already in flight"));
    }
    let values = read_key(transaction, &station)?;
    // Station reports are matched by `requestId` alone (L01.FR.10), so it must stay unique.
    if values
        .iter()
        .any(|record| record.native_request_id == mutation.native_request_id)
    {
        return Err(conflict("firmware requestId already used"));
    }
    if values.len() >= MAX_FIRMWARE_JOBS_201 {
        return Err(conflict("firmware job history capacity exhausted"));
    }
    let revision = values.last().map_or(Ok(1), |record| {
        record
            .revision
            .checked_add(1)
            .ok_or_else(|| conflict("firmware revision exhausted"))
    })?;
    if mutation.deadline <= mutation.admitted_at {
        return Err(conflict("firmware deadline elapsed"));
    }
    let record = FirmwareJobRecord201 {
        station: mutation.station.clone(),
        request_id: mutation.request_id.clone(),
        native_request_id: mutation.native_request_id,
        secure: mutation.secure,
        artifact_reference: mutation.artifact_reference.clone(),
        revision,
        state: FirmwareJobState201::Pending,
        admitted_at: mutation.admitted_at,
        changed_at: mutation.admitted_at,
        deadline: mutation.deadline,
        started: false,
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
    };
    register_release_job(
        transaction,
        &release_job_id(&record.request_id),
        "firmware",
        true,
    )?;
    transaction
        .execute(
            "INSERT INTO firmware201_jobs(station,revision,request_id,native_request_id,inflight,payload)
             VALUES(?1,?2,?3,?4,1,?5)",
            params![
                station,
                i64::try_from(revision).map_err(|_| conflict("firmware revision exhausted"))?,
                mutation.request_id.as_str(),
                mutation.native_request_id,
                crate::codec_firmware201::encode(&record)?
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Stable bounded identity accepted by the release-drain inventory.
pub(crate) fn release_job_id(request_id: &RequestId) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut identity = String::from("firmware201/");
    for byte in Sha256::digest(request_id.as_str().as_bytes()) {
        let _ = write!(identity, "{byte:02x}");
    }
    identity
}

/// Drops resolved revisions whose public result is gone, keeping the newest as station head.
fn prune(connection: &Connection, station: &str) -> Result<(), StorageError> {
    let mut values = read_key(connection, station)?;
    values.pop();
    for record in values.iter().filter(|record| record.state.resolved()) {
        connection
            .execute(
                "DELETE FROM firmware201_jobs WHERE station=?1 AND revision=?2 AND inflight=0
                 AND NOT EXISTS(SELECT 1 FROM command_results WHERE request_id=?3)",
                params![
                    station,
                    i64::try_from(record.revision)
                        .map_err(|_| conflict("firmware revision exhausted"))?,
                    record.request_id.as_str()
                ],
            )
            .map_err(unavailable)?;
    }
    Ok(())
}

pub(super) fn conflict(detail: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::Conflict, detail)
}
