//! Closed, bounded supervisor decision evidence safe for the release read protocol.
use super::{failures, promotion, rollback};
use crate::artifacts::manifest;
use serde::{Deserialize, Serialize};

/// Attribution is intentionally independent from the request shape.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    #[default]
    Operator,
    Supervisor,
}

/// Trusted compatibility result; raw validator errors are never retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compatibility {
    NotChecked,
    Accepted,
    Rejected,
}

/// Trusted production-drain result; no connection or process detail is exposed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Drain {
    NotRequested,
    Granted,
    Rejected,
}

/// Trusted production health state at a promotion decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    NotObserved,
    Probation,
    Healthy,
    Rejected,
}

/// Safe outcome of a promotion decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionOutcome {
    Continuing,
    Rejected,
    RecoveryRequired,
}

/// A supervisor-owned transition. All fields are closed enums or validated digests.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    Promote {
        candidate_digest: String,
        previous_good_digest: Option<String>,
        evidence_digest: Option<String>,
        configuration_digest: Option<String>,
        compatibility: Compatibility,
        drain: Drain,
        health: Health,
        outcome: PromotionOutcome,
    },
    Failure {
        candidate_digest: Option<String>,
        previous_good_digest: Option<String>,
        observation: Option<failures::Observation>,
        decision: failures::Decision,
        trigger_id: Option<u64>,
    },
    Rollback {
        quarantined_digest: Option<String>,
        previous_good_digest: Option<String>,
        step: rollback::Step,
        reason: rollback::Reason,
    },
}

impl Decision {
    pub(super) fn promotion(
        record: &promotion::Record,
        evidence_digest: Option<String>,
        compatibility: Compatibility,
        drain: Drain,
        health: Health,
        outcome: PromotionOutcome,
    ) -> Self {
        Self::Promote {
            candidate_digest: record.candidate.clone(),
            previous_good_digest: Some(record.previous.clone()),
            evidence_digest,
            configuration_digest: Some(record.configuration_digest.clone()),
            compatibility,
            drain,
            health,
            outcome,
        }
    }

    pub(super) fn valid(&self) -> bool {
        let digest = |value: &String| manifest::digest_name(value);
        match self {
            Self::Promote {
                candidate_digest,
                previous_good_digest,
                evidence_digest,
                configuration_digest,
                ..
            } => {
                digest(candidate_digest)
                    && previous_good_digest.iter().all(digest)
                    && evidence_digest.iter().all(digest)
                    && configuration_digest.iter().all(digest)
            }
            Self::Failure {
                candidate_digest,
                previous_good_digest,
                observation,
                trigger_id,
                ..
            } => {
                candidate_digest.iter().all(digest)
                    && previous_good_digest.iter().all(digest)
                    && observation
                        .as_ref()
                        .is_some_and(|observation| observation.id != 0)
                    && trigger_id.is_none_or(|id| id != 0)
            }
            Self::Rollback {
                quarantined_digest,
                previous_good_digest,
                ..
            } => quarantined_digest.iter().all(digest) && previous_good_digest.iter().all(digest),
        }
    }
}
