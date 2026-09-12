//! One automatic fallback per durable incident, independent of management availability.
use super::{Code, Supervisor, failures, preflight, promotion::ProductionProcess};
use crate::{
    activation::{Phase, Transition},
    artifacts::{ArtifactStore, InstallError, filesystem as disk, manifest},
    qualification::{self, Evidence},
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Attempting,
    Restored,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    EligibleFailure,
    NoPreviousGood,
    EligibilityRejected,
    ProcessFailed,
}

/// Retained even after successful fallback. No client can clear an incident or quarantine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub trigger_id: u64,
    pub quarantined_digest: Option<String>,
    pub previous_good: Option<String>,
    pub step: Step,
    pub reason: Reason,
}
impl Record {
    pub(super) fn validate(&self) -> Result<(), InstallError> {
        if self.trigger_id == 0
            || self
                .quarantined_digest
                .iter()
                .chain(&self.previous_good)
                .any(|s| !manifest::digest_name(s))
            || (self.step != Step::RecoveryRequired
                && (self.quarantined_digest.is_none()
                    || self.previous_good.is_none()
                    || self.quarantined_digest == self.previous_good))
        {
            return Err(rejected());
        }
        Ok(())
    }
}

impl Supervisor {
    /// Trusted host composition: persist/classify an authoritative observation, then consume
    /// an eligible decision without a browser, IPC request or human confirmation.
    /// The host must bind observations to its current production invocation as documented by
    /// `observe_failure`. Polling this path must continue when the bridge API is unavailable.
    ///
    /// # Errors
    /// Rejects stale observations, changed failure policy, or uncertain durable writes.
    pub async fn observe_failure_and_rollback(
        &mut self,
        policy: failures::Policy,
        observation: failures::Observation,
        staging: &mut dyn failures::StagingStop,
        process: &mut dyn ProductionProcess,
    ) -> Result<Code, InstallError> {
        let decision = self.observe_failure(policy, observation, staging)?;
        if decision == failures::Decision::RollbackRequired {
            Ok(self.rollback_automatically(process).await)
        } else if decision == failures::Decision::RecoveryRequired {
            Ok(Code::RecoveryRequired)
        } else {
            Ok(Code::Ok)
        }
    }

    /// Consume a persisted rollback decision, also after reboot before a new observation.
    /// Intent commits before any stop/switch/start. Interrupted attempts require operator
    /// recovery instead of retrying an uncertain start or switching back to the failed version.
    pub async fn rollback_automatically(&mut self, process: &mut dyn ProductionProcess) -> Code {
        if self.ledger.needs_recovery() {
            return Code::RecoveryRequired;
        }
        let Some(failure) = self.ledger.status().failures.as_ref() else {
            return Code::ActivationBlocked;
        };
        if failure.decision != failures::Decision::RollbackRequired {
            return if failure.decision == failures::Decision::RecoveryRequired {
                Code::RecoveryRequired
            } else {
                Code::ActivationBlocked
            };
        }
        if let Some(record) = &self.ledger.status().rollback {
            return if record.step == Step::Restored {
                Code::Ok
            } else {
                Code::RecoveryRequired
            };
        }
        let Some(trigger) = &failure.trigger else {
            return Code::RecoveryRequired;
        };
        let state = self.activation.state();
        let mut record = Record {
            trigger_id: trigger.observation.id,
            quarantined_digest: state.production.as_ref().map(|p| p.digest.clone()),
            previous_good: state.previous_good.clone(),
            step: Step::RecoveryRequired,
            reason: Reason::NoPreviousGood,
        };
        if record.previous_good.is_some() && record.previous_good != record.quarantined_digest {
            record.reason = Reason::EligibilityRejected;
            if self.rollback_start().is_ok() {
                record.step = Step::Attempting;
                record.reason = Reason::EligibleFailure;
            }
        }
        if self.ledger.record_rollback(record.clone()).is_err() {
            return Code::StorageFailure;
        }
        if record.step == Step::RecoveryRequired {
            return Code::RecoveryRequired;
        }
        let result = async {
            tokio::time::timeout(DEADLINE, process.stop_and_confirm())
                .await
                .map_err(|_| rejected())??;
            // Recheck after the complete process group stopped; current committed WAL is
            // included by read-only SQLite validation. No operational worker/migration opens.
            let start = self.rollback_start()?;
            let policy = self.preflight_policy.as_ref().ok_or_else(rejected)?;
            uob_storage_adapter::backup::validate_current(
                &start.operational_database,
                uob_storage_adapter::backup::Limits {
                    maximum_bytes: policy.maximum_backup_bytes,
                    expected_schema_version: policy.expected_formats.operational_sqlite.get(),
                    timeout: DEADLINE,
                },
            )
            .map_err(|_| rejected())?;
            preflight::validate_candidate(&start.binary, policy, Instant::now() + DEADLINE)?;
            // Revalidate configuration identity after the independent configuration checker.
            let start = self.rollback_start()?;
            self.activation
                .transition(Transition::BeginRollback, &self.policy)?;
            tokio::time::timeout(DEADLINE, process.start_and_confirm(start))
                .await
                .map_err(|_| rejected())??;
            self.activation
                .transition(Transition::FinishRollback, &self.policy)
        }
        .await;
        record.step = if result.is_ok() {
            Step::Restored
        } else {
            Step::RecoveryRequired
        };
        if result.is_err() {
            record.reason = Reason::ProcessFailed;
        }
        if self.ledger.record_rollback(record).is_err() {
            Code::StorageFailure
        } else if result.is_ok() {
            Code::Ok
        } else {
            Code::RecoveryRequired
        }
    }

    fn rollback_start(&self) -> Result<super::promotion::Start, InstallError> {
        let state = self.activation.state();
        let production = state.production.as_ref().ok_or_else(rejected)?;
        let previous = state.previous_good.as_ref().ok_or_else(rejected)?;
        let promotion = self
            .ledger
            .status()
            .promotion
            .as_ref()
            .ok_or_else(rejected)?;
        if !matches!(
            production.phase,
            Phase::Promoting | Phase::Probation | Phase::Healthy
        ) || promotion.candidate != production.digest
            || &promotion.previous != previous
        {
            return Err(rejected());
        }
        let recorded = self
            .ledger
            .status()
            .qualification
            .as_ref()
            .ok_or_else(rejected)?;
        if recorded.candidate_digest != production.digest {
            return Err(rejected());
        }
        let inbox = self.state_directory.join("evidence");
        let bytes = disk::bounded_read(
            &inbox.join(format!("{}.json", recorded.evidence_digest)),
            qualification::EVIDENCE_LIMIT,
        )?;
        if qualification::digest(&bytes) != recorded.evidence_digest {
            return Err(rejected());
        }
        let signature =
            disk::bounded_read(&inbox.join(format!("{}.sig", recorded.evidence_digest)), 64)?;
        let evidence: Evidence = serde_json::from_slice(&bytes)?;
        let policy = self.preflight_policy.as_ref().ok_or_else(rejected)?;
        if evidence.compatibility.resulting_versions != policy.expected_formats
            || policy.expected_formats != self.policy.current_formats
        {
            return Err(rejected());
        }
        let store = ArtifactStore::open(&self.store)?;
        let old = store.verify_installed(previous, &self.policy)?;
        let new = store.verify_installed(&production.digest, &self.policy)?;
        // Evidence was accepted for this promotion. Its age does not expire the rollback
        // window; signatures, identities, current trust/security and format gates still apply.
        qualification::verify(
            &bytes,
            &signature,
            self.qualification_policy.as_ref().ok_or_else(rejected)?,
            &self.policy,
            &old.manifest,
            &new.manifest,
            evidence.soak_finished_unix_seconds,
        )?;
        drop(store);
        // Identity projection: unchanged current config must be understood by the fallback.
        // Unsupported config changes require explicit recovery, never copying an old config.
        self.production_start(previous)
    }

    pub(super) fn rollback_needs_recovery(&self) -> bool {
        self.ledger
            .status()
            .rollback
            .as_ref()
            .is_some_and(|r| r.step != Step::Restored)
    }
}
fn rejected() -> InstallError {
    InstallError::Rejected("automatic rollback rejected")
}

#[cfg(test)]
mod tests;
