use super::{Code, Supervisor};
use crate::{
    activation::{Phase, Transition},
    artifacts::{ArtifactStore, InstallError, filesystem as disk},
    qualification::{self, EVIDENCE_LIMIT, Policy, Qualified},
};
use std::time::{SystemTime, UNIX_EPOCH};

impl Supervisor {
    /// Enables qualification using administrator-owned configuration, never IPC input.
    /// Omission keeps qualification disabled. Reloading/restarting rechecks evidence.
    ///
    /// # Errors
    /// Rejects invalid trust/matrix policy or an unsafe evidence inbox directory.
    pub fn with_qualification_policy(mut self, policy: Policy) -> Result<Self, InstallError> {
        policy.validate()?;
        disk::directory(&self.state_directory.join("evidence"))?;
        self.qualification_policy = Some(policy);
        Ok(self)
    }

    pub(super) fn current_qualification(&self) -> Option<Qualified> {
        if self.ledger.needs_recovery() {
            return None;
        }
        let recorded = self.ledger.status().qualification.as_ref()?;
        let candidate = self.activation.state().candidate.as_ref()?;
        if candidate.phase != Phase::Qualified || candidate.digest != recorded.candidate_digest {
            return None;
        }
        self.verify_evidence(&recorded.candidate_digest, &recorded.evidence_digest)
            .ok()
    }

    fn verify_evidence(&self, digest: &str, reference: &str) -> Result<Qualified, InstallError> {
        let reject = || InstallError::Rejected("qualification unavailable");
        let policy = self.qualification_policy.as_ref().ok_or_else(reject)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| reject())?
            .as_secs();
        let store = ArtifactStore::open(&self.store)?;
        if disk::bounded_read(&self.store.join("candidate"), 64)? != digest.as_bytes() {
            return Err(reject());
        }
        let production = self
            .activation
            .state()
            .production
            .as_ref()
            .ok_or_else(reject)?;
        if !matches!(production.phase, Phase::Healthy | Phase::PreviousGood)
            || production.digest == digest
        {
            return Err(reject());
        }
        let previous = store.verify_installed(&production.digest, &self.policy)?;
        let candidate = store.verify_installed(digest, &self.policy)?;
        let inbox = self.state_directory.join("evidence");
        disk::directory(&inbox)?;
        let bytes = disk::bounded_read(&inbox.join(format!("{reference}.json")), EVIDENCE_LIMIT)?;
        let signature = disk::bounded_read(&inbox.join(format!("{reference}.sig")), 64)?;
        if qualification::digest(&bytes) != reference {
            return Err(reject());
        }
        qualification::verify(
            &bytes,
            &signature,
            policy,
            &self.policy,
            &previous.manifest,
            &candidate.manifest,
            now,
        )
    }

    pub(super) fn qualify(&mut self, digest: &str, reference: &str) -> Code {
        let mut run = || -> Result<(), InstallError> {
            self.verify_evidence(digest, reference)?;
            self.activation.observe_installed(&self.policy)?;
            if self
                .activation
                .state()
                .candidate
                .as_ref()
                .is_none_or(|c| c.digest != digest)
            {
                return Err(InstallError::Rejected(
                    "candidate changed during qualification",
                ));
            }
            if self
                .activation
                .state()
                .candidate
                .as_ref()
                .is_some_and(|c| c.phase == Phase::Installed)
            {
                self.activation
                    .transition(Transition::BeginStaging, &self.policy)?;
            }
            if self
                .activation
                .state()
                .candidate
                .as_ref()
                .is_some_and(|c| c.phase == Phase::Qualified)
            {
                return Ok(());
            }
            self.activation
                .transition(Transition::Qualify, &self.policy)
        };
        match run() {
            Ok(()) => Code::Ok,
            Err(_) => Code::EvidenceRejected,
        }
    }
}
