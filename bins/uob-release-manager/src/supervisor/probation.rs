//! Production evidence supplied by a trusted host collector, never by release IPC.
use super::{Supervisor, promotion::Step};
use crate::{
    activation::{Phase, Transition},
    artifacts::{InstallError, manifest},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MINIMUM_SECONDS: u64 = 24 * 60 * 60;

/// All checks are mandatory. Missing measurements are failures, not healthy defaults.
/// The host evaluates resource limits using the configured production deployment profile.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    CoreProgress,
    StorageProgress,
    Readiness,
    MemoryBudget,
    CpuBudget,
    ResponseLatency,
}
impl Check {
    const REQUIRED: [Self; 6] = [
        Self::CoreProgress,
        Self::StorageProgress,
        Self::Readiness,
        Self::MemoryBudget,
        Self::CpuBudget,
        Self::ResponseLatency,
    ];
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub required_seconds: u64,
    pub maximum_sample_gap_seconds: u64,
    /// Digest of the administrator-owned health/resource thresholds and collector profile.
    pub profile_digest: String,
}
impl Policy {
    fn validate(&self) -> Result<(), InstallError> {
        if self.required_seconds < MINIMUM_SECONDS
            || self.required_seconds > 30 * MINIMUM_SECONDS
            || !(1..=300).contains(&self.maximum_sample_gap_seconds)
            || !manifest::digest_name(&self.profile_digest)
        {
            return Err(rejected());
        }
        Ok(())
    }
}

/// IDs increase durably across collector restarts. Uptime is a monotonic production-process
/// clock; invocation changes on every process restart. UTC alone can never earn credit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: u64,
    pub unix_seconds: u64,
    pub uptime_seconds: u64,
    pub invocation: String,
    pub candidate: String,
    pub configuration_digest: String,
    pub checks: BTreeMap<Check, bool>,
}
impl Observation {
    fn passed(&self) -> bool {
        Check::REQUIRED
            .iter()
            .all(|check| self.checks.get(check) == Some(&true))
    }
}

/// Constant-sized evidence persists before the activation journal can advance to healthy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub policy: Policy,
    pub started_unix_seconds: u64,
    pub verified_seconds: u64,
    pub interrupted_intervals: u64,
    pub last: Observation,
}
impl State {
    pub(super) fn validate(&self) -> Result<(), InstallError> {
        self.policy.validate()?;
        if self.last.id == 0
            || self.started_unix_seconds > self.last.unix_seconds
            || self.verified_seconds > self.policy.required_seconds
            || [
                &self.last.invocation,
                &self.last.candidate,
                &self.last.configuration_digest,
            ]
            .into_iter()
            .any(|value| !manifest::digest_name(value))
        {
            return Err(rejected());
        }
        Ok(())
    }
    fn observe(&mut self, next: Observation, continuous: bool) -> Result<(), InstallError> {
        if next.id <= self.last.id || next.unix_seconds < self.last.unix_seconds {
            return Err(rejected());
        }
        let elapsed = next.uptime_seconds.checked_sub(self.last.uptime_seconds);
        let utc_elapsed = next.unix_seconds - self.last.unix_seconds;
        let same_process = next.invocation == self.last.invocation;
        let covered = continuous
            && same_process
            && self.last.passed()
            && next.passed()
            && elapsed.is_some_and(|seconds| {
                seconds > 0 && seconds <= self.policy.maximum_sample_gap_seconds
            })
            && utc_elapsed <= self.policy.maximum_sample_gap_seconds;
        if covered {
            self.verified_seconds = self
                .verified_seconds
                .saturating_add(elapsed.unwrap_or(0).min(utc_elapsed))
                .min(self.policy.required_seconds);
        } else {
            self.interrupted_intervals = self.interrupted_intervals.saturating_add(1);
            // Failed checks and missing coverage invalidate the current healthy run.
            // A supervisor restart preserves already committed evidence, but earns no gap time.
            if continuous || !next.passed() || !same_process {
                self.verified_seconds = 0;
            }
        }
        self.last = next;
        self.validate()
    }
    #[must_use]
    pub fn complete(&self) -> bool {
        self.verified_seconds >= self.policy.required_seconds && self.last.passed()
    }
}

impl Supervisor {
    /// Records fresh, authenticated host measurements for the journal-owned production release.
    /// The collector must verify the running artifact/configuration and process invocation,
    /// and evaluate every check against `profile_digest`. No public request supplies evidence.
    /// Restart downtime and wall-clock jumps cannot substitute for measured runtime.
    ///
    /// # Errors
    /// Rejects stale IDs, changed policy/identity, failed storage, or recovery-required state.
    pub fn observe_probation(
        &mut self,
        policy: Policy,
        observation: Observation,
    ) -> Result<bool, InstallError> {
        policy.validate()?;
        if self.ledger.needs_recovery()
            || self.promotion_needs_recovery()
            || self.failure_blocks_activation()
        {
            return Err(rejected());
        }
        let promotion = self
            .ledger
            .status()
            .promotion
            .as_ref()
            .ok_or_else(rejected)?;
        let production = self
            .activation
            .state()
            .production
            .as_ref()
            .ok_or_else(rejected)?;
        if promotion.step != Step::Probation
            || observation.candidate != promotion.candidate
            || observation.configuration_digest != promotion.configuration_digest
            || production.digest != observation.candidate
            || !matches!(production.phase, Phase::Probation | Phase::Healthy)
        {
            return Err(rejected());
        }
        let evidence = match self.ledger.status().probation.clone() {
            Some(mut evidence) => {
                if evidence.policy != policy {
                    return Err(rejected());
                }
                evidence.observe(observation, self.probation_continuous)?;
                evidence
            }
            None => State {
                policy,
                started_unix_seconds: observation.unix_seconds,
                verified_seconds: 0,
                interrupted_intervals: 0,
                last: observation,
            },
        };
        evidence.validate()?;
        // Once healthy, this API no longer accumulates probation; the failure policy owns
        // subsequent degradation. Retrying after a committed healthy transition is harmless.
        if production.phase == Phase::Healthy {
            return Ok(true);
        }
        let complete = evidence.complete();
        self.ledger.record_probation(evidence)?;
        self.probation_continuous = true;
        if complete {
            self.activation
                .transition(Transition::MarkHealthy, &self.policy)?;
        }
        Ok(complete)
    }
}
fn rejected() -> InstallError {
    InstallError::Rejected("production probation evidence rejected")
}

#[cfg(test)]
mod tests;
