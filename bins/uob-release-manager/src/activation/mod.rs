//! Crash-recoverable release observations and exclusive activation ownership.
//! The journal does not authorize or start production: qualification, idle admission,
//! process stop/start and probation evidence must be supplied by their policy owners.
mod disk;
mod model;

use crate::artifacts::{ArtifactStore, InstallError, InstallPolicy, filesystem};
pub use model::{Phase, Release, State, Transition};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

/// Lifetime ownership of activation for one artifact store. Never unlink its lock.
/// A service-control adapter must retain this owner across stop, switch and start,
/// and must confirm the old unit stopped before `BeginPromotion`/`BeginRollback`.
/// The service's independent state/runtime locks continue to exclude duplicate daemons.
pub struct ActivationJournal {
    root: PathBuf,
    state: State,
    poisoned: bool,
    _owner: File,
}

impl ActivationJournal {
    /// Recovers a durable intent before exposing state or allowing another transition.
    /// Existing pointers are verified but do not imply qualification or health.
    ///
    /// # Errors
    /// Rejects competing activation owners, unsafe metadata, corrupt artifacts and
    /// inconsistent pointers; recovery never executes a process or imports test data.
    pub fn open(root: &Path, policy: &InstallPolicy) -> Result<Self, InstallError> {
        filesystem::directory(root)?;
        let owner = disk::owner(root)?;
        let store = ArtifactStore::open_for_recovery(root)?;
        disk::recover(root, &store, policy)?;
        let state = disk::read_state(root)?.unwrap_or_default();
        disk::verify(root, &state, &store, policy)?;
        Ok(Self {
            root: root.to_owned(),
            state,
            poisoned: false,
            _owner: owner,
        })
    }

    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Records the installed candidate after signature verification. New installation
    /// invalidates old staging/qualification observations; no staging execution is implied.
    ///
    /// # Errors
    /// Rejects replacement during activation/probation and changed production pointers.
    pub fn observe_installed(&mut self, policy: &InstallPolicy) -> Result<(), InstallError> {
        self.ready()?;
        let store = ArtifactStore::open(&self.root)?;
        disk::verify(&self.root, &self.state, &store, policy)?;
        let candidate = disk::pointer(&self.root, "candidate")?;
        if candidate.as_deref() == self.state.candidate.as_ref().map(|c| c.digest.as_str()) {
            return Ok(());
        }
        if self.state.busy() {
            return Err(InstallError::Rejected("activation in progress"));
        }
        let mut next = self.state.clone();
        next.candidate = candidate.map(|digest| Release {
            digest,
            phase: Phase::Installed,
        });
        next.sequence = next
            .sequence
            .checked_add(1)
            .ok_or(InstallError::Rejected("release sequence exhausted"))?;
        self.commit(&store, next, policy)
    }

    /// Persists a policy-owned observation. This is not a public authorization API.
    /// `Qualify` and `MarkHealthy` record independently verified evidence; they do not
    /// substitute for qualification or the required production probation interval.
    ///
    /// # Errors
    /// Rejects invalid ordering, replaced candidates, invalid artifacts and I/O failures.
    /// After an I/O failure the owner must be dropped and reopened to recover.
    pub fn transition(
        &mut self,
        transition: Transition,
        policy: &InstallPolicy,
    ) -> Result<(), InstallError> {
        self.ready()?;
        let store = ArtifactStore::open(&self.root)?;
        disk::verify(&self.root, &self.state, &store, policy)?;
        if disk::pointer(&self.root, "candidate")?.as_deref()
            != self.state.candidate.as_ref().map(|c| c.digest.as_str())
        {
            return Err(InstallError::Rejected("candidate observation expired"));
        }
        let next = self.state.advance(transition)?;
        self.commit(&store, next, policy)
    }

    fn ready(&self) -> Result<(), InstallError> {
        if self.poisoned {
            Err(InstallError::Rejected("activation recovery required"))
        } else {
            Ok(())
        }
    }

    fn commit(
        &mut self,
        store: &ArtifactStore,
        next: State,
        policy: &InstallPolicy,
    ) -> Result<(), InstallError> {
        next.validate()?;
        for digest in next.digests() {
            store.verify_installed(digest, policy)?;
        }
        self.poisoned = true;
        disk::commit(&self.root, &self.state, &next)?;
        self.state = next;
        self.poisoned = false;
        Ok(())
    }
}

// Detect production pointer drift before an ordinary store open can prune artifacts.
pub(crate) fn check_store_consistency(root: &Path) -> Result<(), InstallError> {
    if let Some(state) = disk::read_state(root)? {
        disk::verify_pointers(root, &state)?;
    }
    Ok(())
}

// Installer must not replace/prune the candidate used by an unfinished activation.
pub(crate) fn installation_blocked(root: &Path) -> Result<bool, InstallError> {
    Ok(disk::read_state(root)?.is_some_and(|state| state.busy()))
}

#[cfg(test)]
mod tests;
