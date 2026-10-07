//! Durable OCPP 1.6 firmware jobs. Each unresolved job holds one `release_jobs` row, written
//! and removed in the same transaction as the job state, so a release drain sees exactly the
//! physical firmware work that is not yet confirmed finished.
use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};
use rusqlite::{Connection, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    FirmwareJobMutation16, FirmwareJobRecord16, FirmwareObservation16, FirmwareObservationKind16,
    FirmwareStore16, FirmwareVariant16, MAX_FIRMWARE_JOBS_16, StorageError, StorageErrorCode,
    StorageFuture,
};
use uob_contracts::{FirmwareJobState16, RequestId, ResourceRef, UtcTimestamp};
mod observations;
mod recovery;
mod transitions;
pub(crate) mod validation;
pub(crate) use observations::observe;
pub(crate) use recovery::maintain;
pub(crate) use transitions::finish;

/// Mirrors the release-drain inventory bound.
const MAX_RELEASE_JOBS: i64 = 128;

impl<C, E, D, R> FirmwareStore16 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn firmware_jobs_16(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<FirmwareJobRecord16>> {
        self.request(|reply| Request::FirmwareJobs16(station, reply))
    }
    fn recover_firmware_jobs_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainFirmware16(now, true, reply))
    }
    fn expire_firmware_jobs_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainFirmware16(now, false, reply))
    }
}

pub(crate) fn create(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS firmware16_jobs(
        station TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0),
        request_id TEXT NOT NULL UNIQUE, native_request_id INTEGER,
        inflight INTEGER NOT NULL CHECK(inflight IN (0,1)), payload TEXT NOT NULL,
        PRIMARY KEY(station,revision));
        CREATE UNIQUE INDEX IF NOT EXISTS firmware16_owner ON firmware16_jobs(station) WHERE inflight=1;
        CREATE INDEX IF NOT EXISTS firmware16_native ON firmware16_jobs(station,native_request_id,revision);",
        )
        .map_err(unavailable)
}

pub(crate) fn read(
    connection: &Connection,
    station: &ResourceRef,
) -> Result<Vec<FirmwareJobRecord16>, StorageError> {
    read_key(connection, &crate::snapshots::station_key(station)?)
}

pub(super) fn read_key(
    connection: &Connection,
    station: &str,
) -> Result<Vec<FirmwareJobRecord16>, StorageError> {
    let mut statement = connection
        .prepare("SELECT payload FROM firmware16_jobs WHERE station=?1 ORDER BY revision LIMIT 65")
        .map_err(unavailable)?;
    let values = statement
        .query_map([station], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .map(|row| crate::codec_firmware16::decode(&row.map_err(unavailable)?))
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() > MAX_FIRMWARE_JOBS_16 {
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
    record: &FirmwareJobRecord16,
    inflight: Option<bool>,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&record.station)?;
    let revision =
        i64::try_from(record.revision).map_err(|_| conflict("firmware revision exhausted"))?;
    let changed = connection
        .execute(
            "UPDATE firmware16_jobs SET payload=?3,inflight=COALESCE(?4,inflight)
             WHERE station=?1 AND revision=?2",
            params![
                station,
                revision,
                crate::codec_firmware16::encode(record)?,
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

pub(crate) fn reserve(
    transaction: &Transaction<'_>,
    mutation: &FirmwareJobMutation16,
) -> Result<(), StorageError> {
    validation::validate_mutation(transaction, mutation)?;
    let station = crate::snapshots::station_key(&mutation.station)?;
    observe(
        transaction,
        &FirmwareObservation16 {
            station: mutation.station.clone(),
            observed_at: mutation.admitted_at,
            kind: FirmwareObservationKind16::Expiry,
        },
    )?;
    prune(transaction, &station)?;
    let inflight: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM firmware16_jobs WHERE station=?1 AND inflight=1)",
            [&station],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if inflight {
        return Err(conflict("station firmware request already in flight"));
    }
    let values = read_key(transaction, &station)?;
    match mutation.variant {
        FirmwareVariant16::Legacy => {
            // OCPP 1.6 defines no cancellation; only an unconfirmed job may be replaced.
            if values.iter().any(|record| {
                !record.state.resolved()
                    && !matches!(
                        record.state,
                        FirmwareJobState16::TimedOut | FirmwareJobState16::Uncertain
                    )
            }) {
                return Err(conflict("firmware update already active"));
            }
        }
        FirmwareVariant16::Signed { request_id } => {
            if values
                .iter()
                .any(|record| record.variant == FirmwareVariant16::Signed { request_id })
            {
                return Err(conflict("firmware requestId already used"));
            }
        }
    }
    if values.len() >= MAX_FIRMWARE_JOBS_16 {
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
    let record = FirmwareJobRecord16 {
        station: mutation.station.clone(),
        request_id: mutation.request_id.clone(),
        variant: mutation.variant,
        artifact_reference: mutation.artifact_reference.clone(),
        revision,
        state: FirmwareJobState16::Pending,
        admitted_at: mutation.admitted_at,
        changed_at: mutation.admitted_at,
        deadline: mutation.deadline,
        started: false,
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
    };
    register_release_job(transaction, &release_job_id(&record.request_id), true)?;
    transaction
        .execute(
            "INSERT INTO firmware16_jobs(station,revision,request_id,native_request_id,inflight,payload)
             VALUES(?1,?2,?3,?4,1,?5)",
            params![
                station,
                i64::try_from(revision).map_err(|_| conflict("firmware revision exhausted"))?,
                mutation.request_id.as_str(),
                match mutation.variant {
                    FirmwareVariant16::Legacy => None,
                    FirmwareVariant16::Signed { request_id } => Some(request_id),
                },
                crate::codec_firmware16::encode(&record)?
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Registration must succeed before dispatch; a full inventory refuses the new job.
pub(crate) fn register_release_job(
    connection: &Connection,
    id: &str,
    bounded: bool,
) -> Result<(), StorageError> {
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM release_jobs", [], |row| row.get(0))
        .map_err(unavailable)?;
    if bounded && count >= MAX_RELEASE_JOBS {
        return Err(conflict("stateful job inventory full"));
    }
    connection
        .execute(
            "INSERT OR IGNORE INTO release_jobs(id, kind) VALUES (?1, 'firmware')",
            [id],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Stable bounded identity accepted by the release-drain inventory.
pub(crate) fn release_job_id(request_id: &RequestId) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut identity = String::from("firmware16/");
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
                "DELETE FROM firmware16_jobs WHERE station=?1 AND revision=?2 AND inflight=0
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
