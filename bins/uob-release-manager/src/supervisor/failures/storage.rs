use super::model::Staging;
use super::{Decision, State};
use crate::artifacts::InstallError;

impl State {
    pub(crate) fn validate(&self) -> Result<(), InstallError> {
        let fail = || InstallError::Rejected("invalid persistent failure state");
        let policy = self.policy.ok_or_else(fail)?;
        policy.validate()?;
        let last = self.last.ok_or_else(fail)?;
        if (self.started_at.is_some() != (self.invocation != 0))
            || self.pending_pressure.is_some_and(|o| {
                self.started_at.is_none_or(|at| o.at_seconds < at)
                    || o.at_seconds > last.at_seconds
                    || o.id > last.id
            })
            || (self.staging == Staging::Pending && self.pending_pressure.is_none())
            || self.trigger.as_ref().is_some_and(|a| {
                a.observation.id > last.id
                    || a.observation.at_seconds > last.at_seconds
                    || !matches!(
                        a.decision,
                        Decision::RollbackRequired | Decision::RecoveryRequired
                    )
            })
            || self.audit.is_empty()
            || self.audit.len() > 64
            || self.exits.len() > policy.exit_count
            || self.readiness_failures.len() > policy.readiness_count
            || self.exits.windows(2).any(|w| w[0] > w[1])
            || self.readiness_failures.windows(2).any(|w| w[0] >= w[1])
            || self
                .exits
                .iter()
                .chain(&self.readiness_failures)
                .any(|at| *at > last.at_seconds)
            || self.started_at.is_some_and(|at| at > last.at_seconds)
            || self
                .audit
                .last()
                .is_none_or(|a| a.observation != last || a.decision != self.decision)
            || self.audit.iter().any(|a| a.observation.id == 0)
            || self.audit.windows(2).any(|w| {
                w[0].observation.id >= w[1].observation.id
                    || w[0].observation.at_seconds > w[1].observation.at_seconds
            })
            || (self.staging == Staging::Pending
                && !matches!(
                    self.decision,
                    Decision::StopStaging | Decision::RecoveryRequired
                ))
        {
            return Err(fail());
        }
        Ok(())
    }
}
