//! Production preflight is repeated per promotion request, never inferred from backup presence.
use super::{Code, Supervisor};
use crate::{
    FormatVersions,
    artifacts::{ArtifactStore, InstallError, filesystem as disk},
    qualification::{self, Evidence},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Administrator-owned production inputs. IPC callers cannot choose paths or limits.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub configuration: PathBuf,
    pub operational_database: PathBuf,
    pub expected_formats: FormatVersions,
    pub service_uid: u32,
    pub service_gid: u32,
    pub maximum_backup_bytes: u64,
    pub timeout_seconds: u64,
}

/// Disaster recovery evidence, deliberately outside activation pointers and the ledger.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupMetadata {
    pub format: String,
    pub candidate_digest: String,
    pub previous_digest: String,
    pub qualification_digest: String,
    pub production_configuration_digest: String,
    pub expected_formats: FormatVersions,
    pub bytes: u64,
}

impl Supervisor {
    /// Configures preflight without reading production secrets or creating a backup.
    ///
    /// # Errors
    /// Rejects unsafe paths, resource limits and non-private backup directories.
    pub fn with_preflight_policy(mut self, policy: Policy) -> Result<Self, InstallError> {
        if !(1..=300).contains(&policy.timeout_seconds)
            || policy.maximum_backup_bytes == 0
            || policy.maximum_backup_bytes > self.policy.backup_reserve_bytes
            || !policy.configuration.is_absolute()
            || !policy.operational_database.is_absolute()
            || policy.configuration == policy.operational_database
            || policy.operational_database.starts_with(&self.store)
            || policy
                .operational_database
                .starts_with(&self.state_directory)
        {
            return Err(InstallError::Rejected(
                "invalid production preflight policy",
            ));
        }
        let directory = self.state_directory.join("backups");
        disk::directory(&directory)?;
        if fs::metadata(&directory)?.permissions().mode() & 0o777 != 0o700 {
            return Err(InstallError::Rejected("backup directory must be private"));
        }
        self.preflight_policy = Some(policy);
        Ok(self)
    }

    pub(super) fn preflight(&self, digest: &str) -> Code {
        match self.run_preflight(digest) {
            Ok(()) => Code::ActivationBlocked, // idle/drain/process control remain separate gates
            Err(_) => Code::PreflightRejected,
        }
    }

    fn run_preflight(&self, digest: &str) -> Result<(), InstallError> {
        let fail = || InstallError::Rejected("production preflight rejected");
        let policy = self.preflight_policy.as_ref().ok_or_else(fail)?;
        let qualified = self
            .current_qualification()
            .filter(|q| q.candidate_digest == digest)
            .ok_or_else(fail)?;
        let evidence_bytes = disk::bounded_read(
            &self
                .state_directory
                .join("evidence")
                .join(format!("{}.json", qualified.evidence_digest)),
            qualification::EVIDENCE_LIMIT,
        )?;
        if qualification::digest(&evidence_bytes) != qualified.evidence_digest {
            return Err(fail());
        }
        let evidence: Evidence = serde_json::from_slice(&evidence_bytes)?;
        if evidence.compatibility.resulting_versions != policy.expected_formats {
            return Err(fail());
        }
        // Keep the installer/activation lock across execution and backup.
        let store = ArtifactStore::open(&self.store)?;
        if disk::bounded_read(&self.store.join("candidate"), 64)? != digest.as_bytes() {
            return Err(fail());
        }
        let candidate = store.verify_installed(digest, &self.policy)?;
        let production = self
            .activation
            .state()
            .production
            .as_ref()
            .ok_or_else(fail)?;
        let previous = store.verify_installed(&production.digest, &self.policy)?;
        crate::NormalPromotionGate::evaluate(
            &self.policy.security,
            &previous.manifest.compatibility,
            &candidate.manifest.compatibility,
            &evidence.compatibility,
        )
        .map_err(|_| fail())?;
        let config = disk::bounded_read(&policy.configuration, 1024 * 1024)?;
        let deadline = Instant::now() + Duration::from_secs(policy.timeout_seconds);
        validate_candidate(&candidate.path.join("bin/uob"), policy, deadline)?;
        let directory = self.state_directory.join("backups");
        disk::directory(&directory)?;
        // One retained slot bounds disk use. Existing or interrupted work requires
        // explicit disaster-recovery retention handling, never automatic overwrite.
        if fs::read_dir(&directory)?.next().is_some() {
            return Err(fail());
        }
        disk::capacity(&directory, policy.maximum_backup_bytes, 3)?;
        let database = directory.join("production.sqlite");
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(fail)?;
        let bytes = uob_storage_adapter::backup::create(
            &policy.operational_database,
            &database,
            uob_storage_adapter::backup::Limits {
                maximum_bytes: policy.maximum_backup_bytes,
                expected_schema_version: self.policy.current_formats.operational_sqlite.get(),
                timeout: remaining,
            },
        )
        .map_err(|_| fail())?;
        // A changed configuration never earns a valid backup/preflight record.
        if disk::bounded_read(&policy.configuration, 1024 * 1024)? != config {
            return Err(fail());
        }
        let metadata = BackupMetadata {
            format: "uob-production-backup-v1".into(),
            candidate_digest: digest.into(),
            previous_digest: production.digest.clone(),
            qualification_digest: qualified.evidence_digest,
            production_configuration_digest: qualification::digest(&config),
            expected_formats: policy.expected_formats,
            bytes,
        };
        disk::write_new(
            &directory.join("metadata.json"),
            &serde_json::to_vec(&metadata)?,
        )?;
        disk::sync_dir(&directory)?;
        Ok(())
    }
}

fn validate_candidate(
    binary: &Path,
    policy: &Policy,
    deadline: Instant,
) -> Result<(), InstallError> {
    let mut child = Command::new(binary)
        .args(["config", "check", "--config"])
        .arg(&policy.configuration)
        .arg("--secrets")
        .env_clear()
        .current_dir("/")
        .uid(policy.service_uid)
        .gid(policy.service_gid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(InstallError::Rejected("candidate configuration rejected")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(InstallError::Rejected(
                    "candidate validation unavailable or timed out",
                ));
            }
        }
    }
}
