use super::{Code, Record, Request, Status};
use crate::artifacts::{InstallError, filesystem as disk, manifest};
use std::{
    fs,
    fs::File,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub struct Ledger {
    root: PathBuf,
    state: Status,
    recovery: bool,
    _lock: File,
}

impl Ledger {
    pub fn open(root: &Path) -> Result<Self, InstallError> {
        disk::directory(root)?;
        if fs::metadata(root)?.permissions().mode() & 0o777 != 0o700 {
            return Err(InstallError::Rejected(
                "supervisor state must be private mode 0700",
            ));
        }
        let lock_path = root.join("owner.lock");
        let lock = match disk::open(&lock_path, true, true) {
            Ok(file) => file,
            Err(InstallError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                disk::open(&lock_path, true, false)?
            }
            Err(error) => return Err(error),
        };
        lock.try_lock()
            .map_err(|_| InstallError::Rejected("supervisor already running"))?;
        let path = root.join("state.json");
        let state = match fs::symlink_metadata(&path) {
            Ok(_) => serde_json::from_slice(&disk::bounded_read(&path, 65536)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Status::default(),
            Err(e) => return Err(e.into()),
        };
        validate(&state)?;
        // A partial publication never becomes authoritative on restart. Keep status
        // readable, block mutation, and retain evidence for explicit recovery.
        let recovery = match fs::symlink_metadata(root.join("state.next")) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            root: root.to_owned(),
            state,
            recovery,
            _lock: lock,
        })
    }

    pub const fn status(&self) -> &Status {
        &self.state
    }
    pub const fn needs_recovery(&self) -> bool {
        self.recovery
    }

    pub fn record(&mut self, uid: u32, request: Request, result: Code) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        next.sequence = next
            .sequence
            .checked_add(1)
            .ok_or(InstallError::Rejected("supervisor sequence exhausted"))?;
        if result != Code::Ok {
            next.failed_operations = next.failed_operations.saturating_add(1);
        }
        if let Request::Stage { digest } = &request {
            // A failed recheck invalidates the previous staging observation.
            next.staged_verified_digest = (result == Code::Ok).then(|| digest.clone());
            next.qualification = None;
        }
        if let Request::Qualify {
            digest,
            evidence_digest,
        } = &request
        {
            next.qualification = (result == Code::Ok).then(|| crate::qualification::Qualified {
                candidate_digest: digest.clone(),
                evidence_digest: evidence_digest.clone(),
                // Recomputed from authenticated evidence for every status/promotion read.
                pi_measurements_digest: None,
            });
        }
        next.last_operation = Some(Record {
            sequence: next.sequence,
            uid,
            request,
            result,
        });
        self.persist(next)
    }

    pub fn record_failures(
        &mut self,
        failures: super::failures::State,
    ) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        next.failures = Some(failures);
        self.persist(next)
    }

    pub fn record_promotion(
        &mut self,
        promotion: super::promotion::Record,
    ) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        next.promotion = Some(promotion);
        self.persist(next)
    }

    fn persist(&mut self, next: Status) -> Result<(), InstallError> {
        validate(&next)?;
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > 65536 {
            return Err(InstallError::Rejected("supervisor ledger exceeds bound"));
        }
        let temporary = self.root.join("state.next");
        self.recovery = true;
        disk::write_new(&temporary, &bytes)?;
        fs::rename(&temporary, self.root.join("state.json"))?;
        disk::sync_dir(&self.root)?;
        self.state = next;
        self.recovery = false;
        Ok(())
    }
}

fn validate(state: &Status) -> Result<(), InstallError> {
    if let Some(promotion) = &state.promotion {
        promotion.validate()?;
    }
    if let Some(failures) = &state.failures {
        failures.validate()?;
    }
    let bad_digest = state
        .staged_verified_digest
        .as_ref()
        .is_some_and(|d| !manifest::digest_name(d));
    let bad_record = state.last_operation.as_ref().is_some_and(|r| {
        r.sequence != state.sequence || matches!(&r.request,
            Request::Stage { digest } | Request::Promote { digest } | Request::Qualify { digest, .. } if !manifest::digest_name(digest))
            || matches!(&r.request, Request::Qualify { evidence_digest, .. } if !manifest::digest_name(evidence_digest))
    });
    if state.qualification.as_ref().is_some_and(|q| {
        !manifest::digest_name(&q.candidate_digest) || !manifest::digest_name(&q.evidence_digest)
    }) || bad_digest
        || bad_record
        || state.failed_operations > state.sequence
        || (state.sequence == 0) != state.last_operation.is_none()
    {
        return Err(InstallError::Rejected("invalid supervisor ledger"));
    }
    Ok(())
}
