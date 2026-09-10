//! Trusted, bounded evidence for an exact immutable candidate; never an activation command.
use crate::{
    NormalPromotionGate, RollbackCycleEvidence, SchemaVersion,
    artifacts::{BundleManifest, InstallError, InstallPolicy, manifest::digest_name},
};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const EVIDENCE_LIMIT: usize = 64 * 1024;
pub const MINIMUM_SOAK_SECONDS: u64 = 24 * 60 * 60;

/// Provisioned independently of artifact publishers and IPC callers. A key is bound
/// to one trusted harness identity; it must only sign results it actually observed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    pub producer: String,
    pub ed25519_key: Vec<u8>,
}

/// Administrator-owned acceptance matrix and exact representative staging inputs.
/// Digests identify versioned configuration, sanitized dataset and soak profile files.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub authorities: Vec<Authority>,
    pub configuration_digest: String,
    pub dataset_digest: String,
    pub soak_profile_digest: String,
    pub required_suites: BTreeSet<String>,
    pub maximum_evidence_age_seconds: u64,
}

impl Policy {
    /// # Errors
    /// Rejects empty trust/matrix, invalid identities and unbounded policy values.
    pub fn validate(&self) -> Result<(), InstallError> {
        let mut producers = BTreeSet::new();
        let mut keys = BTreeSet::new();
        if self.authorities.is_empty()
            || self.authorities.len() > 16
            || self.authorities.iter().any(|a| {
                !identifier(&a.producer)
                    || a.ed25519_key.len() != 32
                    || !producers.insert(&a.producer)
                    || !keys.insert(&a.ed25519_key)
            })
            || !digest_name(&self.configuration_digest)
            || !digest_name(&self.dataset_digest)
            || !digest_name(&self.soak_profile_digest)
            || self.required_suites.is_empty()
            || self.required_suites.len() > 64
            || self.required_suites.iter().any(|s| !identifier(s))
            || self.maximum_evidence_age_seconds == 0
            || self.maximum_evidence_age_seconds > 30 * MINIMUM_SOAK_SECONDS
        {
            return Err(InstallError::Rejected("invalid qualification policy"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub id: String,
    pub passed: bool,
    /// Content-addressed detailed result retained by the trusted harness.
    pub result_digest: String,
}

/// Signature covers these exact JSON bytes, not a client-provided success flag.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// Domain separation from artifact manifests and other signed documents.
    pub format: String,
    pub producer: String,
    pub candidate_digest: String,
    pub source_commit: String,
    pub configuration_schema: SchemaVersion,
    pub configuration_digest: String,
    pub dataset_digest: String,
    pub soak_profile_digest: String,
    pub soak_started_unix_seconds: u64,
    pub soak_finished_unix_seconds: u64,
    pub soak_passed: bool,
    pub suites: Vec<Suite>,
    /// Full old-new-old assertions are evaluated by the existing compatibility gate.
    pub compatibility: RollbackCycleEvidence,
    /// Optional signed Pi hardware measurement report; generic ARM64 is insufficient.
    pub pi_measurements_digest: Option<String>,
}

/// Safe proof reference. This observation must be reverified against current policy
/// and installed bytes before promotion; neither deserialization nor a journal phase
/// alone grants eligibility.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qualified {
    pub candidate_digest: String,
    pub evidence_digest: String,
    pub pi_measurements_digest: Option<String>,
}

/// Verify signed results before evaluating claims. `now` is supervisor-owned UTC;
/// the producer must measure continuous soak duration, including interruptions.
///
/// # Errors
/// Rejects untrusted, stale, failed, incomplete or mismatched evidence. Performs no
/// publication, process execution, pointer update or database operation.
pub fn verify(
    encoded: &[u8],
    signature: &[u8],
    policy: &Policy,
    install: &InstallPolicy,
    previous: &BundleManifest,
    candidate: &BundleManifest,
    now: u64,
) -> Result<Qualified, InstallError> {
    policy.validate()?;
    let reject = || InstallError::Rejected("qualification evidence rejected");
    if encoded.len() > EVIDENCE_LIMIT || signature.len() != 64 {
        return Err(reject());
    }
    // Verify before parsing. The authenticated producer must match the verifying key.
    let authority = policy
        .authorities
        .iter()
        .find(|a| {
            UnparsedPublicKey::new(&ED25519, &a.ed25519_key)
                .verify(encoded, signature)
                .is_ok()
        })
        .ok_or_else(reject)?;
    let evidence: Evidence = serde_json::from_slice(encoded)?;
    if evidence.format != "uob-qualification-v1"
        || evidence.producer != authority.producer
        || evidence.candidate_digest != candidate.compatibility.artifact_digest.as_str()
        || evidence.source_commit != candidate.source_commit
        || evidence.configuration_schema != install.current_formats.configuration
        || evidence.configuration_schema != evidence.compatibility.resulting_versions.configuration
        || evidence.configuration_digest != policy.configuration_digest
        || evidence.dataset_digest != policy.dataset_digest
        || evidence.soak_profile_digest != policy.soak_profile_digest
        || !evidence.soak_passed
        || evidence
            .soak_finished_unix_seconds
            .checked_sub(evidence.soak_started_unix_seconds)
            .is_none_or(|duration| duration < MINIMUM_SOAK_SECONDS)
        || now
            .checked_sub(evidence.soak_finished_unix_seconds)
            .is_none_or(|age| age > policy.maximum_evidence_age_seconds)
        || evidence
            .pi_measurements_digest
            .as_ref()
            .is_some_and(|d| !digest_name(d))
    {
        return Err(reject());
    }
    let mut suites = BTreeSet::new();
    if evidence.suites.len() > 64
        || evidence.suites.iter().any(|s| {
            !identifier(&s.id)
                || !s.passed
                || !digest_name(&s.result_digest)
                || !suites.insert(s.id.clone())
        })
        || !policy.required_suites.is_subset(&suites)
    {
        return Err(reject());
    }
    NormalPromotionGate::evaluate(
        &install.security,
        &previous.compatibility,
        &candidate.compatibility,
        &evidence.compatibility,
    )
    .map_err(|_| reject())?;
    Ok(Qualified {
        candidate_digest: evidence.candidate_digest,
        evidence_digest: digest(encoded),
        pi_measurements_digest: evidence.pi_measurements_digest,
    })
}

#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    crate::artifacts::filesystem::hex(&Sha256::digest(bytes))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
