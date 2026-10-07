//! Durable OCPP 2.0.1 log upload jobs. Each unresolved job holds one `release_jobs` row,
//! written and removed in the same transaction as the job state, so a release drain sees
//! exactly the station uploads that are not yet confirmed finished.
use crate::{
    SqliteOperationalStore, configuration::unavailable, firmware16::register_release_job,
    worker::Request,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    DiagnosticsJobMutation201, DiagnosticsJobRecord201, DiagnosticsObservation201,
    DiagnosticsObservationKind201, DiagnosticsStore201, MAX_DIAGNOSTICS_JOBS_201, StorageError,
    StorageErrorCode, StorageFuture,
};
use uob_contracts::{DiagnosticsJobState201, RequestId, ResourceRef, UtcTimestamp};
mod observations;
mod recovery;
mod transitions;
pub(crate) mod validation;
pub(crate) use observations::observe;
pub(crate) use recovery::maintain;
pub(crate) use transitions::finish;

impl<C, E, D, R> DiagnosticsStore201 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn diagnostics_jobs_201(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<DiagnosticsJobRecord201>> {
        self.request(|reply| Request::DiagnosticsJobs201(station, reply))
    }
    fn bind_diagnostics_upload_201(
        &self,
        request_id: RequestId,
        upload_id: String,
    ) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::BindDiagnosticsUpload201(request_id, upload_id, reply))
    }
    fn recover_diagnostics_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainDiagnostics201(now, true, reply))
    }
    fn expire_diagnostics_jobs_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()> {
        self.request(|reply| Request::MaintainDiagnostics201(now, false, reply))
    }
}

pub(crate) fn create(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS diagnostics201_jobs(
        station TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0),
        request_id TEXT NOT NULL UNIQUE, native_request_id INTEGER NOT NULL,
        inflight INTEGER NOT NULL CHECK(inflight IN (0,1)), payload TEXT NOT NULL,
        PRIMARY KEY(station,revision));
        CREATE UNIQUE INDEX IF NOT EXISTS diagnostics201_owner ON diagnostics201_jobs(station) WHERE inflight=1;
        CREATE INDEX IF NOT EXISTS diagnostics201_native ON diagnostics201_jobs(station,native_request_id,revision);",
        )
        .map_err(unavailable)
}

pub(crate) fn read(
    connection: &Connection,
    station: &ResourceRef,
) -> Result<Vec<DiagnosticsJobRecord201>, StorageError> {
    read_key(connection, &crate::snapshots::station_key(station)?)
}

pub(super) fn read_key(
    connection: &Connection,
    station: &str,
) -> Result<Vec<DiagnosticsJobRecord201>, StorageError> {
    let mut statement = connection
        .prepare(
            "SELECT payload FROM diagnostics201_jobs WHERE station=?1 ORDER BY revision LIMIT 65",
        )
        .map_err(unavailable)?;
    let values = statement
        .query_map([station], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .map(|row| crate::codec_diagnostics201::decode(&row.map_err(unavailable)?))
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() > MAX_DIAGNOSTICS_JOBS_201 {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "log upload job history exceeds bounded capacity",
        ));
    }
    Ok(values)
}

/// Persists one revision and keeps the release-drain inventory in the same transaction.
pub(super) fn save(
    connection: &Connection,
    record: &DiagnosticsJobRecord201,
    inflight: Option<bool>,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&record.station)?;
    let revision =
        i64::try_from(record.revision).map_err(|_| conflict("log job revision exhausted"))?;
    let changed = connection
        .execute(
            "UPDATE diagnostics201_jobs SET payload=?3,inflight=COALESCE(?4,inflight)
             WHERE station=?1 AND revision=?2",
            params![
                station,
                revision,
                crate::codec_diagnostics201::encode(record)?,
                inflight
            ],
        )
        .map_err(unavailable)?;
    if changed != 1 {
        return Err(conflict("log job owner changed"));
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
    mutation: &DiagnosticsJobMutation201,
) -> Result<(), StorageError> {
    validation::validate_mutation(transaction, mutation)?;
    let station = crate::snapshots::station_key(&mutation.station)?;
    observe(
        transaction,
        &DiagnosticsObservation201 {
            station: mutation.station.clone(),
            observed_at: mutation.admitted_at,
            kind: DiagnosticsObservationKind201::Expiry,
        },
    )?;
    prune(transaction, &station)?;
    let inflight: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM diagnostics201_jobs WHERE station=?1 AND inflight=1)",
            [&station],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if inflight {
        return Err(conflict("station log request already in flight"));
    }
    let values = read_key(transaction, &station)?;
    // Reports are matched by `requestId` alone (N01.FR.07), so a retained one cannot be reused.
    // A new request may cancel an ongoing upload (N01.FR.12); the station says so in its reply.
    if values
        .iter()
        .any(|record| record.native_request_id == mutation.native_request_id)
    {
        return Err(conflict("log requestId already used"));
    }
    if values.len() >= MAX_DIAGNOSTICS_JOBS_201 {
        return Err(conflict("log job history capacity exhausted"));
    }
    let revision = values.last().map_or(Ok(1), |record| {
        record
            .revision
            .checked_add(1)
            .ok_or_else(|| conflict("log job revision exhausted"))
    })?;
    if mutation.deadline <= mutation.admitted_at {
        return Err(conflict("log job deadline elapsed"));
    }
    let record = DiagnosticsJobRecord201 {
        station: mutation.station.clone(),
        request_id: mutation.request_id.clone(),
        native_request_id: mutation.native_request_id,
        log_type: mutation.log_type,
        revision,
        state: DiagnosticsJobState201::Pending,
        admitted_at: mutation.admitted_at,
        changed_at: mutation.admitted_at,
        deadline: mutation.deadline,
        started: false,
        upload_id: None,
        last_status: None,
        last_status_at: None,
        notifications: 0,
        upload: None,
    };
    register_release_job(
        transaction,
        &release_job_id(&record.request_id),
        "diagnostics",
        true,
    )?;
    transaction
        .execute(
            "INSERT INTO diagnostics201_jobs(station,revision,request_id,native_request_id,inflight,payload)
             VALUES(?1,?2,?3,?4,1,?5)",
            params![
                station,
                i64::try_from(revision).map_err(|_| conflict("log job revision exhausted"))?,
                mutation.request_id.as_str(),
                mutation.native_request_id,
                crate::codec_diagnostics201::encode(&record)?
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Binds the provider destination to a pending, unsent job before dispatch may send it.
pub(crate) fn bind_upload(
    connection: &mut Connection,
    request_id: &RequestId,
    upload_id: String,
) -> Result<(), StorageError> {
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let payload: Option<(String, bool)> = transaction
        .query_row(
            "SELECT payload,inflight FROM diagnostics201_jobs WHERE request_id=?1",
            [request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, true)) = payload else {
        return Err(conflict("log job is not awaiting dispatch"));
    };
    let mut record = crate::codec_diagnostics201::decode(&payload)?;
    if record.state != DiagnosticsJobState201::Pending || record.upload_id.is_some() {
        return Err(conflict("log job is not awaiting dispatch"));
    }
    record.upload_id = Some(upload_id);
    // From here the request may reach the station, so restart must treat it as possibly sent.
    record.started = true;
    crate::codec_diagnostics201::validate_upload_id(record.upload_id.as_deref())?;
    save(&transaction, &record, None)?;
    transaction.commit().map_err(unavailable)
}

/// Stable bounded identity accepted by the release-drain inventory.
pub(crate) fn release_job_id(request_id: &RequestId) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut identity = String::from("diagnostics201/");
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
                "DELETE FROM diagnostics201_jobs WHERE station=?1 AND revision=?2 AND inflight=0
                 AND NOT EXISTS(SELECT 1 FROM command_results WHERE request_id=?3)",
                params![
                    station,
                    i64::try_from(record.revision)
                        .map_err(|_| conflict("log job revision exhausted"))?,
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
