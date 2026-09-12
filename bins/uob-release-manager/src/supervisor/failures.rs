//! Trusted host observations, deliberately unavailable to release IPC clients.
mod evaluate;
mod model;
mod storage;
use super::Supervisor;
use crate::artifacts::InstallError;
pub use model::{Decision, Observation, Policy, Signal, State};

/// Fixed host operation: stop the entire staging slice and confirm it is inactive.
/// Implementations must bound their deadline and never accept a caller-selected unit.
pub trait StagingStop {
    /// Returns true only after confirmed shutdown; false includes unavailable control.
    fn stop_and_confirm(&mut self) -> bool;
}

impl Supervisor {
    /// Records a trusted service-manager observation before returning a policy decision.
    /// The host supplies UTC seconds and strictly increasing observation IDs across reboot.
    /// Never feed this method unauthenticated HTTP, charger or release-client input.
    ///
    /// # Errors
    /// Rejects invalid/reordered observations, changed policy and failed durable writes.
    /// After an uncertain write, all mutation requires explicit recovery.
    pub fn observe_failure(
        &mut self,
        policy: Policy,
        observation: Observation,
        staging: &mut dyn StagingStop,
    ) -> Result<Decision, InstallError> {
        if self.ledger.needs_recovery() {
            return Err(InstallError::Rejected("supervisor requires recovery"));
        }
        let mut next = self.ledger.status().failures.clone().unwrap_or_default();
        let mut decision = next.observe(policy, observation)?;
        // The incident latch remains intact after fallback. Any internal fallback failure
        // now requires recovery, never another version change; external degradation is harmless.
        if self
            .ledger
            .status()
            .rollback
            .as_ref()
            .is_some_and(|r| r.step == super::rollback::Step::Restored)
            && matches!(
                observation.signal,
                Signal::Watchdog
                    | Signal::Oom
                    | Signal::InternalReadinessFailure
                    | Signal::StartupPending
                    | Signal::FatalInvariant { .. }
                    | Signal::Exit {
                        desired_running: true,
                        unexpected: true
                    }
            )
        {
            decision = Decision::RecoveryRequired;
            next.decision = decision;
            if let Some(last) = next.audit.last_mut() {
                last.decision = decision;
            }
        }
        self.ledger.record_failures(next.clone())?;
        if decision == Decision::StopStaging {
            // Persist intent first. On a crash the next observation retries this idempotent
            // operation, never treating the interrupted call as successful shutdown.
            next.staging_stopped(staging.stop_and_confirm());
            self.ledger.record_failures(next.clone())?;
        }
        Ok(next.decision)
    }
}
