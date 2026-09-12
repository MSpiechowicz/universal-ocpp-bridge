//! Authorized activation at the live production worker's frozen boundary.
//!
//! The host supplies its existing drain transport and process controller. Opening another
//! SQLite worker is not a substitute for the production process's `ReleaseDrainPort`.
use super::{Code, Permission, Request, Response, Supervisor, preflight};
use crate::{
    activation::{Phase, Transition},
    artifacts::{ArtifactStore, InstallError, filesystem as disk, manifest},
    drain::{StagingStopPort, wait_for_idle_boundary},
    qualification,
};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use uob_application::release_drain::ReleaseDrainPort;

pub type ProcessFuture<'a> = Pin<Box<dyn Future<Output = Result<(), InstallError>> + Send + 'a>>;

/// Only supervisor-validated production inputs reach process execution. No staging paths,
/// imported commands, backup path, arbitrary arguments or client-supplied environment exist.
#[derive(Clone, Debug)]
pub struct Start {
    pub digest: String,
    pub binary: PathBuf,
    pub configuration: PathBuf,
    pub configuration_digest: String,
    pub operational_database: PathBuf,
    pub uid: u32,
    pub gid: u32,
}

/// Trusted host adapter for exactly one production service and its complete process group.
/// Stop must disable pending restarts, close sockets, and confirm the entire old group exited.
/// Start must use the provided production inputs, retain the service's independent state/runtime
/// locks, and await core readiness with ordinary persisted transaction/command reconciliation.
/// Cancellation/timeout is uncertain: implementations must retain ownership of any queued job;
/// a subsequent stop must also stop that job. Never return success for a merely queued action.
pub trait ProductionProcess: Send {
    fn stop_and_confirm(&mut self) -> ProcessFuture<'_>;
    fn start_and_confirm(&mut self, start: Start) -> ProcessFuture<'_>;
}

/// Host-owned live connections, never constructed from the public IPC request body.
pub struct Host<'a> {
    pub drain: Arc<dyn ReleaseDrainPort>,
    pub staging: &'a dyn StagingStopPort,
    pub production: &'a mut dyn ProductionProcess,
    pub maintenance_window: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Stopping,
    Starting,
    Probation,
    RecoveredPrevious,
    RecoveryRequired,
}

/// One bounded durable attempt. A restart cannot turn its old drain into a fresh permit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub candidate: String,
    pub previous: String,
    pub configuration_digest: String,
    pub production_inputs_digest: String,
    pub database_device: u64,
    pub database_inode: u64,
    pub step: Step,
    pub recovery_attempted: bool,
}
impl Record {
    pub(super) fn validate(&self) -> Result<(), InstallError> {
        if self.candidate == self.previous
            || [
                &self.candidate,
                &self.previous,
                &self.configuration_digest,
                &self.production_inputs_digest,
            ]
            .into_iter()
            .any(|d| !manifest::digest_name(d))
        {
            return Err(InstallError::Rejected(
                "invalid production activation record",
            ));
        }
        Ok(())
    }
}

const START_DEADLINE: Duration = Duration::from_secs(30);

impl Supervisor {
    pub(super) fn promotion_needs_recovery(&self) -> bool {
        self.ledger.status().promotion.as_ref().is_some_and(|r| {
            matches!(
                r.step,
                Step::Stopping | Step::Starting | Step::RecoveredPrevious | Step::RecoveryRequired
            )
        })
    }

    pub(super) fn promotion_blocks_mutation(&self) -> bool {
        self.ledger.status().promotion.as_ref().is_some_and(|r| {
            r.step != Step::Probation
                || self
                    .activation
                    .state()
                    .production
                    .as_ref()
                    .is_none_or(|p| p.phase != Phase::Healthy)
        })
    }

    /// Completes one authorized normal promotion. Failed readiness leaves recovery evidence;
    /// it never marks a candidate healthy or performs automatic rollback (#160).
    /// The synchronous IPC `handle` stays fail-closed when no live host is attached.
    pub async fn promote_at_idle(&mut self, uid: u32, digest: String, host: Host<'_>) -> Response {
        if !self.can_activate(uid) {
            return Response::code(Code::Forbidden);
        }
        if !manifest::digest_name(&digest) {
            return Response::code(Code::InvalidRequest);
        }
        if self.activation_blocked() {
            return Response::code(Code::RecoveryRequired);
        }
        if host.maintenance_window.is_zero() || host.maintenance_window > Duration::from_hours(24) {
            return Response::code(Code::InvalidRequest);
        }
        if self
            .current_qualification()
            .is_none_or(|q| q.candidate_digest != digest)
        {
            return Response::code(Code::QualificationRequired);
        }
        let code = self.activate(&digest, host).await;
        self.record_activation(uid, digest, code)
    }

    async fn activate(&mut self, digest: &str, host: Host<'_>) -> Code {
        let Some(policy) = self.preflight_policy.as_ref() else {
            return Code::PreflightRejected;
        };
        let Ok(database) = std::fs::symlink_metadata(&policy.operational_database) else {
            return Code::PreflightRejected;
        };
        let Ok(inputs) = serde_json::to_vec(policy) else {
            return Code::PreflightRejected;
        };
        let Ok(backup) = self.run_preflight(digest) else {
            return Code::PreflightRejected;
        };
        let Ok(boundary) =
            wait_for_idle_boundary(host.drain, host.staging, host.maintenance_window).await
        else {
            return Code::ActivationBlocked;
        };
        // A long drain must not extend qualification validity or authorize a replaced artifact.
        if self
            .current_qualification()
            .is_none_or(|q| q.candidate_digest != digest)
        {
            return Code::EvidenceRejected;
        }
        let record = Record {
            candidate: digest.into(),
            previous: backup.previous_digest,
            configuration_digest: backup.production_configuration_digest,
            production_inputs_digest: qualification::digest(&inputs),
            database_device: database.dev(),
            database_inode: database.ino(),
            step: Step::Stopping,
            recovery_attempted: false,
        };
        if self.ledger.record_promotion(record).is_err() {
            return Code::StorageFailure;
        }
        let result = async {
            tokio::time::timeout(boundary.remaining(), boundary.validate())
                .await
                .map_err(|_| rejected())?
                .map_err(|_| rejected())?;
            self.production_start(digest)?; // recheck the exact validated configuration before stop
            tokio::time::timeout(boundary.remaining(), host.production.stop_and_confirm())
                .await
                .map_err(|_| rejected())??;
            // A slow/queued stop never earns an artifact switch after the seal expired.
            if boundary.remaining().is_zero() {
                return Err(rejected());
            }
            if self
                .current_qualification()
                .is_none_or(|q| q.candidate_digest != digest)
            {
                return Err(rejected());
            }
            let start = self.production_start(digest)?;
            self.activation
                .transition(Transition::BeginPromotion, &self.policy)?;
            self.set_promotion_step(Step::Starting)?;
            // The old group has exited. Its process-local drain may now be released.
            drop(boundary);
            tokio::time::timeout(START_DEADLINE, host.production.start_and_confirm(start))
                .await
                .map_err(|_| rejected())??;
            self.activation
                .transition(Transition::BeginProbation, &self.policy)?;
            self.set_promotion_step(Step::Probation)
        }
        .await;
        self.activation_result(&result)
    }

    /// Recovers an interrupted stop/switch/start once using the currently journal-owned artifact.
    /// Kernel-derived activation permission is required. Never recreates a drain, changes the
    /// selected version, restores a database, or retries failed recovery after another reboot.
    pub async fn recover_activation(
        &mut self,
        uid: u32,
        process: &mut dyn ProductionProcess,
    ) -> Response {
        if !self.can_activate(uid) {
            return Response::code(Code::Forbidden);
        }
        if self.ledger.needs_recovery() || self.failure_blocks_activation() {
            return Response::code(Code::RecoveryRequired);
        }
        let Some(mut record) = self.ledger.status().promotion.clone() else {
            return Response::code(Code::InvalidRequest);
        };
        if record.recovery_attempted || !matches!(record.step, Step::Stopping | Step::Starting) {
            return Response::code(Code::RecoveryRequired);
        }
        record.recovery_attempted = true;
        let digest = record.candidate.clone();
        if self.ledger.record_promotion(record).is_err() {
            return Response::code(Code::StorageFailure);
        }
        let result = async {
            let production = self
                .activation
                .state()
                .production
                .clone()
                .ok_or_else(rejected)?;
            let record = self
                .ledger
                .status()
                .promotion
                .as_ref()
                .ok_or_else(rejected)?;
            let candidate = production.digest == record.candidate;
            if !(candidate && matches!(production.phase, Phase::Promoting | Phase::Probation)
                || production.digest == record.previous
                    && matches!(production.phase, Phase::Healthy | Phase::PreviousGood))
            {
                return Err(rejected());
            }
            // Stop even if readiness was observed before the crash; no duplicate production group.
            tokio::time::timeout(START_DEADLINE, process.stop_and_confirm())
                .await
                .map_err(|_| rejected())??;
            let start = self.production_start(&production.digest)?;
            let policy = self.preflight_policy.as_ref().ok_or_else(rejected)?;
            preflight::validate_candidate(&start.binary, policy, Instant::now() + START_DEADLINE)?;
            tokio::time::timeout(START_DEADLINE, process.start_and_confirm(start))
                .await
                .map_err(|_| rejected())??;
            if candidate {
                if production.phase == Phase::Promoting {
                    self.activation
                        .transition(Transition::BeginProbation, &self.policy)?;
                }
                self.set_promotion_step(Step::Probation)
            } else {
                // Before intent publication, only the old artifact may resume. Fresh promotion
                // needs operator recovery and a new preflight/drain; no expired seal is reused.
                self.set_promotion_step(Step::RecoveredPrevious)
            }
        }
        .await;
        let code = self.activation_result(&result);
        self.record_activation(uid, digest, code)
    }

    fn production_start(&self, digest: &str) -> Result<Start, InstallError> {
        let policy = self.preflight_policy.as_ref().ok_or_else(rejected)?;
        let record = self
            .ledger
            .status()
            .promotion
            .as_ref()
            .ok_or_else(rejected)?;
        if qualification::digest(&disk::bounded_read(&policy.configuration, 1024 * 1024)?)
            != record.configuration_digest
        {
            return Err(rejected());
        }
        let database = std::fs::symlink_metadata(&policy.operational_database)?;
        if !database.is_file()
            || database.dev() != record.database_device
            || database.ino() != record.database_inode
            || qualification::digest(&serde_json::to_vec(policy)?)
                != record.production_inputs_digest
        {
            return Err(rejected());
        }
        let artifact = ArtifactStore::open(&self.store)?.verify_installed(digest, &self.policy)?;
        Ok(Start {
            digest: digest.into(),
            binary: artifact.path.join("bin/uob"),
            configuration: policy.configuration.clone(),
            configuration_digest: record.configuration_digest.clone(),
            operational_database: policy.operational_database.clone(),
            uid: policy.service_uid,
            gid: policy.service_gid,
        })
    }
    fn can_activate(&self, uid: u32) -> bool {
        self.grants
            .iter()
            .any(|g| g.uid == uid && g.permissions.contains(&Permission::Activate))
    }
    pub(super) fn failure_blocks_activation(&self) -> bool {
        self.ledger.status().failures.as_ref().is_some_and(|s| {
            matches!(
                s.decision,
                super::failures::Decision::RollbackRequired
                    | super::failures::Decision::RecoveryRequired
            )
        })
    }
    fn activation_blocked(&self) -> bool {
        self.ledger.needs_recovery()
            || self.promotion_blocks_mutation()
            || self.failure_blocks_activation()
    }
    fn set_promotion_step(&mut self, step: Step) -> Result<(), InstallError> {
        let mut record = self
            .ledger
            .status()
            .promotion
            .clone()
            .ok_or_else(rejected)?;
        record.step = step;
        self.ledger.record_promotion(record)
    }
    fn activation_result(&mut self, result: &Result<(), InstallError>) -> Code {
        if result.is_ok() {
            return Code::Ok;
        }
        if self.set_promotion_step(Step::RecoveryRequired).is_err() {
            Code::StorageFailure
        } else {
            Code::RecoveryRequired
        }
    }
    fn record_activation(&mut self, uid: u32, digest: String, code: Code) -> Response {
        match self.ledger.record(uid, Request::Promote { digest }, code) {
            Ok(()) => Response::code(code),
            Err(_) => Response::code(Code::StorageFailure),
        }
    }
}
fn rejected() -> InstallError {
    InstallError::Rejected("production activation requires recovery")
}
