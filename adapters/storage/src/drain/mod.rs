//! Serialized with the same worker that owns every operational commit.
mod port;
use crate::{
    codec::{self, EncodedWrite},
    configuration::unavailable,
};
use rusqlite::Connection;
use std::time::{Duration, Instant};
use uob_application::{
    StorageError, StorageErrorCode,
    release_drain::{DrainId, DrainObservation, ReleaseJobKind},
};
use uob_contracts::TransactionState;

pub(crate) enum Operation {
    Begin(Duration),
    Observe(DrainId),
    Seal(DrainObservation),
    Cancel(DrainId),
    StartJob(String, ReleaseJobKind),
    FinishJob(String),
}
pub(crate) enum Outcome {
    Window(DrainId),
    Observation(DrainObservation),
    Done,
}
#[derive(Default)]
pub(crate) struct Drain {
    window: Option<Window>,
    revision: u64,
}
struct Window {
    id: DrainId,
    deadline: Instant,
    sealed: bool,
}

impl Drain {
    pub(crate) fn expire(&mut self) {
        if self
            .window
            .as_ref()
            .is_some_and(|w| Instant::now() >= w.deadline)
        {
            self.window = None;
        }
    }
    pub(crate) fn admission_status(
        &mut self,
        mut status: uob_application::StorageRetentionStatus,
    ) -> uob_application::StorageRetentionStatus {
        self.expire();
        if self.window.is_some() {
            status.new_session_admission = uob_application::StorageAdmissionState::ReleaseDraining;
        }
        status
    }
    pub(crate) fn changed(&mut self) -> Result<(), StorageError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| busy("drain revision exhausted"))?;
        Ok(())
    }
    pub(crate) fn check_remote_write(&mut self, start: bool) -> Result<(), StorageError> {
        self.expire();
        if self.window.as_ref().is_some_and(|w| w.sealed || start) {
            self.changed()?;
            return Err(busy("remote command blocked by release drain"));
        }
        self.changed()
    }
    pub(crate) fn check_write(&mut self, write: &EncodedWrite) -> Result<(), StorageError> {
        self.expire();
        if let Some(window) = &self.window {
            if window.sealed {
                self.changed()?;
                return Err(busy(
                    "release idle boundary sealed; retry after maintenance",
                ));
            }
            // A station report is evidence of possibly autonomous charging: retain it during
            // drain so it invalidates idle. Only bridge-issued starts are denied here.
            if write.requires_start_admission {
                return Err(busy("new starts disabled for release drain"));
            }
        }
        Ok(())
    }
    pub(crate) fn operation(
        &mut self,
        db: &Connection,
        op: Operation,
    ) -> Result<Outcome, StorageError> {
        self.expire();
        match op {
            Operation::Begin(duration) => {
                if duration.is_zero() || duration > Duration::from_hours(24) {
                    return Err(StorageError::new(
                        StorageErrorCode::InvalidRequest,
                        "maintenance window must be within one day",
                    ));
                }
                if self.window.is_some() {
                    return Err(busy("release drain already owned"));
                }
                let id = DrainId::new();
                self.window = Some(Window {
                    id: id.clone(),
                    deadline: Instant::now() + duration,
                    sealed: false,
                });
                Ok(Outcome::Window(id))
            }
            Operation::Observe(id) => {
                self.require(&id)?;
                Ok(Outcome::Observation(inventory(db, id, self.revision)?))
            }
            Operation::Seal(observed) => {
                self.require(&observed.window)?;
                if observed.revision != self.revision
                    || !inventory(db, observed.window.clone(), self.revision)?.is_idle()
                {
                    return Err(busy(
                        "late state change invalidated release idle observation",
                    ));
                }
                self.window.as_mut().expect("checked window").sealed = true;
                Ok(Outcome::Done)
            }
            Operation::Cancel(id) => {
                // A delayed cancellation must never reopen a newer owner's window.
                if self.window.as_ref().is_some_and(|w| w.id.same_window(&id)) {
                    self.window = None;
                }
                Ok(Outcome::Done)
            }
            Operation::StartJob(id, kind) => {
                valid_id(&id)?;
                if self.window.as_ref().is_some_and(|w| w.sealed) {
                    self.changed()?;
                    return Err(busy("release idle boundary sealed"));
                }
                // Late autonomous/recovered jobs must be recorded even during drain.
                let count: i64 = db
                    .query_row("SELECT COUNT(*) FROM release_jobs", [], |row| row.get(0))
                    .map_err(unavailable)?;
                if count >= 128 {
                    return Err(busy("stateful job inventory full"));
                }
                self.changed()?;
                db.execute(
                    "INSERT INTO release_jobs(id, kind) VALUES (?1, ?2)",
                    rusqlite::params![
                        id,
                        match kind {
                            ReleaseJobKind::Firmware => "firmware",
                            ReleaseJobKind::Certificate => "certificate",
                        }
                    ],
                )
                .map_err(unavailable)?;
                Ok(Outcome::Done)
            }
            Operation::FinishJob(id) => {
                valid_id(&id)?;
                if self.window.as_ref().is_some_and(|w| w.sealed) {
                    self.changed()?;
                    return Err(busy("release idle boundary sealed"));
                }
                self.changed()?;
                db.execute("DELETE FROM release_jobs WHERE id = ?1", [id])
                    .map_err(unavailable)?;
                Ok(Outcome::Done)
            }
        }
    }
    fn require(&self, id: &DrainId) -> Result<(), StorageError> {
        if self.window.as_ref().is_some_and(|w| w.id.same_window(id)) {
            Ok(())
        } else {
            Err(busy(
                "release drain expired or belongs to another process/window",
            ))
        }
    }
}
fn inventory(
    db: &Connection,
    window: DrainId,
    revision: u64,
) -> Result<DrainObservation, StorageError> {
    let mut active_transactions = 0;
    let mut statement = db
        .prepare("SELECT payload FROM station_snapshots")
        .map_err(unavailable)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(unavailable)?;
    for row in rows {
        let snapshot = codec::decode_snapshot(&row.map_err(unavailable)?)?;
        active_transactions += snapshot
            .transactions
            .iter()
            .filter(|t| t.state != TransactionState::Ended)
            .count() as u64;
    }
    let unresolved_commands: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM commands WHERE unresolved = 1",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let stateful_jobs: i64 = db
        .query_row("SELECT COUNT(*) FROM release_jobs", [], |row| row.get(0))
        .map_err(unavailable)?;
    Ok(DrainObservation {
        window,
        revision,
        active_transactions,
        unresolved_commands: u64::try_from(unresolved_commands)
            .map_err(|_| busy("invalid command count"))?,
        stateful_jobs: u64::try_from(stateful_jobs).map_err(|_| busy("invalid job count"))?,
    })
}
fn valid_id(id: &str) -> Result<(), StorageError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_/.".contains(&b))
    {
        Err(StorageError::new(
            StorageErrorCode::InvalidRequest,
            "invalid stateful job identity",
        ))
    } else {
        Ok(())
    }
}
fn busy(detail: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::Busy, detail)
}
