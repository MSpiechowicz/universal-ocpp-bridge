//! Independent local release-control boundary. No application process is required.
pub mod ipc;
mod qualification;
mod storage;

use crate::activation::ActivationJournal;
use crate::artifacts::{ArtifactStore, InstallError, InstallPolicy, filesystem, manifest};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use storage::Ledger;

/// Explicit permissions; activation does not implicitly grant staging or reading.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Stage,
    Activate,
}

/// Administrator-owned UID mapping. The UID comes from the kernel, never JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub uid: u32,
    pub permissions: Vec<Permission>,
}

/// Local protocol v1: only digests can select application artifacts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status {},
    Stage {
        digest: String,
    },
    Qualify {
        digest: String,
        evidence_digest: String,
    },
    Promote {
        digest: String,
    },
    Rollback {},
}

impl Request {
    const fn permission(&self) -> Permission {
        match self {
            Self::Status {} => Permission::Read,
            Self::Stage { .. } | Self::Qualify { .. } => Permission::Stage,
            Self::Promote { .. } | Self::Rollback {} => Permission::Activate,
        }
    }
}

/// Safe result codes do not include raw input, secrets, paths, or OS error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Ok,
    Forbidden,
    InvalidRequest,
    Busy,
    ArtifactRejected,
    QualificationRequired,
    EvidenceRejected,
    ActivationBlocked,
    RecoveryRequired,
    StorageFailure,
}

/// Bounded private request evidence, separate from the later activation state machine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub sequence: u64,
    pub uid: u32,
    pub request: Request,
    pub result: Code,
}

/// Persistent supervisor state does not assert staging execution or charging health.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub sequence: u64,
    pub failed_operations: u64,
    pub staged_verified_digest: Option<String>,
    pub last_operation: Option<Record>,
    #[serde(default)]
    pub qualification: Option<crate::qualification::Qualified>,
}

/// One bounded response. Status is returned only after the read permission check.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub manager_version: String,
    pub code: Code,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

impl Response {
    #[must_use]
    pub fn code(code: Code) -> Self {
        Self {
            protocol: 1,
            manager_version: env!("CARGO_PKG_VERSION").to_owned(),
            code,
            status: None,
        }
    }
}

/// Sole owner of private persistent request/failure state.
pub struct Supervisor {
    ledger: Ledger,
    activation: ActivationJournal,
    grants: Vec<Grant>,
    store: std::path::PathBuf,
    policy: InstallPolicy,
    qualification_policy: Option<crate::qualification::Policy>,
    state_directory: std::path::PathBuf,
}

impl Supervisor {
    /// Opens private state and validates administrator configuration.
    ///
    /// # Errors
    /// Rejects unsafe directories, duplicate/unbounded grants and concurrent owners.
    pub fn open(
        state: &Path,
        store: &Path,
        policy: InstallPolicy,
        grants: Vec<Grant>,
    ) -> Result<Self, InstallError> {
        let mut users = BTreeSet::new();
        if grants.is_empty()
            || grants.len() > 64
            || grants.iter().any(|grant| {
                grant.permissions.is_empty()
                    || grant.permissions.len() > 3
                    || !users.insert(grant.uid)
            })
        {
            return Err(InstallError::Rejected("invalid supervisor grants"));
        }
        filesystem::directory(store)?;
        if state == store || state.starts_with(store) || store.starts_with(state) {
            return Err(InstallError::Rejected("supervisor state must be separate"));
        }
        Ok(Self {
            ledger: Ledger::open(state)?,
            activation: ActivationJournal::open(store, &policy)?,
            grants,
            store: store.to_owned(),
            policy,
            qualification_policy: None,
            state_directory: state.to_owned(),
        })
    }

    /// Handles a request after transport-derived authentication.
    /// No request can change a pointer, execute code, or replace the supervisor.
    pub fn handle(&mut self, uid: u32, request: Request) -> Response {
        if !self
            .grants
            .iter()
            .any(|grant| grant.uid == uid && grant.permissions.contains(&request.permission()))
        {
            return Response::code(Code::Forbidden);
        }
        if let Request::Status {} = request {
            let mut status = self.ledger.status().clone();
            status.qualification = self.current_qualification();
            return Response {
                status: Some(status),
                ..Response::code(if self.ledger.needs_recovery() {
                    Code::RecoveryRequired
                } else {
                    Code::Ok
                })
            };
        }
        if matches!(&request, Request::Stage { digest } | Request::Promote { digest } | Request::Qualify { digest, .. }
            if !manifest::digest_name(digest))
            || matches!(&request, Request::Qualify { evidence_digest, .. } if !manifest::digest_name(evidence_digest))
        {
            return Response::code(Code::InvalidRequest);
        }
        if self.ledger.needs_recovery() {
            return Response::code(Code::RecoveryRequired);
        }
        let code = match &request {
            Request::Stage { digest } => self.stage(digest),
            Request::Qualify {
                digest,
                evidence_digest,
            } => self.qualify(digest, evidence_digest),
            // Neither operator permission nor signed bytes prove qualification,
            // idle admission, compatible rollback, or activation recovery ownership.
            Request::Promote { digest } => {
                if self
                    .current_qualification()
                    .is_some_and(|q| &q.candidate_digest == digest)
                {
                    // Idle/drain admission and production process control are separate work.
                    Code::ActivationBlocked
                } else {
                    Code::QualificationRequired
                }
            }
            Request::Rollback {} => Code::QualificationRequired,
            Request::Status {} => unreachable!(),
        };
        match self.ledger.record(uid, request, code) {
            Ok(()) => Response::code(code),
            Err(_) => Response::code(Code::StorageFailure),
        }
    }

    fn stage(&self, digest: &str) -> Code {
        let verify = || -> Result<(), InstallError> {
            // Share the installer/disk-admission lock only during this operation.
            let store = ArtifactStore::open(&self.store)?;
            let candidate = filesystem::bounded_read(&self.store.join("candidate"), 64)?;
            if candidate != digest.as_bytes() {
                return Err(InstallError::Rejected("artifact is not the candidate"));
            }
            store.verify_installed(digest, &self.policy)?;
            Ok(())
        };
        match verify() {
            Ok(()) => Code::Ok,
            Err(_) => Code::ArtifactRejected,
        }
    }
}
