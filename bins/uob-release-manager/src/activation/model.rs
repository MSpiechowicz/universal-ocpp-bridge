use crate::artifacts::{InstallError, manifest::digest_name};
use serde::{Deserialize, Serialize};

/// Persisted observations. Policy evidence and process execution are separate gates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Installed,
    Staging,
    Qualified,
    Promoting,
    Probation,
    Healthy,
    Quarantined,
    RollingBack,
    PreviousGood,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub digest: String,
    pub phase: Phase,
}

/// Bounded current state; the in-flight intent retains both sides of one transition.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub sequence: u64,
    pub production: Option<Release>,
    pub previous_good: Option<String>,
    pub candidate: Option<Release>,
}

/// Internal policy observations, never accepted directly from the public IPC protocol.
#[derive(Clone, Copy, Debug)]
pub enum Transition {
    BeginStaging,
    Qualify,
    FailStaging,
    BeginPromotion,
    BeginProbation,
    MarkHealthy,
    BeginRollback,
    FinishRollback,
}

impl State {
    pub(crate) fn validate(&self) -> Result<(), InstallError> {
        if self.digests().any(|digest| !digest_name(digest)) {
            return Err(InstallError::Rejected("invalid release journal digest"));
        }
        if self.production.as_ref().is_some_and(|p| {
            !matches!(
                p.phase,
                Phase::Installed
                    | Phase::Promoting
                    | Phase::Probation
                    | Phase::Healthy
                    | Phase::RollingBack
                    | Phase::PreviousGood
            )
        }) || self
            .candidate
            .as_ref()
            .is_some_and(|c| matches!(c.phase, Phase::RollingBack | Phase::PreviousGood))
        {
            return Err(InstallError::Rejected("invalid release journal phase"));
        }
        if let Some(production) = &self.production {
            if matches!(production.phase, Phase::Promoting | Phase::Probation)
                && self.candidate.as_ref() != Some(production)
            {
                return Err(InstallError::Rejected("activation candidate mismatch"));
            }
            if matches!(production.phase, Phase::RollingBack | Phase::PreviousGood)
                && self.previous_good.as_ref() != Some(&production.digest)
            {
                return Err(InstallError::Rejected("rollback artifact mismatch"));
            }
        }
        Ok(())
    }

    pub(crate) fn digests(&self) -> impl Iterator<Item = &str> {
        self.production
            .iter()
            .map(|r| r.digest.as_str())
            .chain(self.previous_good.iter().map(String::as_str))
            .chain(self.candidate.iter().map(|r| r.digest.as_str()))
    }

    pub(crate) fn busy(&self) -> bool {
        self.production.as_ref().is_some_and(|p| {
            matches!(
                p.phase,
                Phase::Promoting | Phase::Probation | Phase::RollingBack
            )
        })
    }

    pub(crate) fn follows(&self, before: &Self) -> bool {
        let observation = !before.busy()
            && before.production == self.production
            && before.previous_good == self.previous_good
            && before.candidate != self.candidate
            && self
                .candidate
                .as_ref()
                .is_none_or(|c| c.phase == Phase::Installed)
            && before.sequence.checked_add(1) == Some(self.sequence);
        observation
            || [
                Transition::BeginStaging,
                Transition::Qualify,
                Transition::FailStaging,
                Transition::BeginPromotion,
                Transition::BeginProbation,
                Transition::MarkHealthy,
                Transition::BeginRollback,
                Transition::FinishRollback,
            ]
            .into_iter()
            .any(|step| before.advance(step).is_ok_and(|next| next == *self))
    }

    pub(crate) fn advance(&self, transition: Transition) -> Result<Self, InstallError> {
        let mut next = self.clone();
        let candidate = next
            .candidate
            .as_mut()
            .ok_or(InstallError::Rejected("no candidate observation"))?;
        let invalid = || InstallError::Rejected("invalid release transition");
        match transition {
            Transition::BeginStaging if candidate.phase == Phase::Installed => {
                candidate.phase = Phase::Staging;
            }
            Transition::Qualify if candidate.phase == Phase::Staging => {
                candidate.phase = Phase::Qualified;
            }
            Transition::FailStaging
                if matches!(candidate.phase, Phase::Staging | Phase::Qualified) =>
            {
                candidate.phase = Phase::Quarantined;
            }
            Transition::BeginPromotion if candidate.phase == Phase::Qualified && !self.busy() => {
                if let Some(production) = &next.production {
                    if !matches!(production.phase, Phase::Healthy | Phase::PreviousGood) {
                        return Err(invalid());
                    }
                    if production.digest == candidate.digest {
                        return Err(invalid());
                    }
                    next.previous_good = Some(production.digest.clone());
                }
                candidate.phase = Phase::Promoting;
                next.production = Some(candidate.clone());
            }
            Transition::BeginProbation if candidate.phase == Phase::Promoting => {
                candidate.phase = Phase::Probation;
                next.production = Some(candidate.clone());
            }
            Transition::MarkHealthy if candidate.phase == Phase::Probation => {
                candidate.phase = Phase::Healthy;
                next.production = Some(candidate.clone());
            }
            Transition::BeginRollback
                if matches!(
                    candidate.phase,
                    Phase::Promoting | Phase::Probation | Phase::Healthy
                ) =>
            {
                let digest = next.previous_good.clone().ok_or_else(invalid)?;
                candidate.phase = Phase::Quarantined;
                next.production = Some(Release {
                    digest,
                    phase: Phase::RollingBack,
                });
            }
            Transition::FinishRollback
                if next
                    .production
                    .as_ref()
                    .is_some_and(|p| p.phase == Phase::RollingBack) =>
            {
                next.production.as_mut().ok_or_else(invalid)?.phase = Phase::PreviousGood;
            }
            _ => return Err(invalid()),
        }
        next.sequence = self.sequence.checked_add(1).ok_or_else(invalid)?;
        next.validate()?;
        Ok(next)
    }
}
