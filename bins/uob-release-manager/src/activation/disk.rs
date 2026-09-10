use super::{Phase, Release, State};
use crate::artifacts::{ArtifactStore, InstallError, InstallPolicy, filesystem as fsafe, manifest};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

const STATE: &str = ".release-state.json";
const INTENT: &str = ".activation-intent";
const LIMIT: usize = 8192;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    before: State,
    after: State,
}

pub(super) fn owner(root: &Path) -> Result<File, InstallError> {
    let path = root.join(".activation-owner.lock");
    let file = match fsafe::open(&path, true, true) {
        Ok(file) => file,
        Err(InstallError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            fsafe::open(&path, true, false)?
        }
        Err(e) => return Err(e),
    };
    file.try_lock()
        .map_err(|_| InstallError::Rejected("production activation already owned"))?;
    Ok(file)
}

fn present(path: &Path) -> Result<bool, InstallError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn read_state(root: &Path) -> Result<Option<State>, InstallError> {
    if !present(&root.join(STATE))? {
        return Ok(None);
    }
    let state: State = serde_json::from_slice(&fsafe::bounded_read(&root.join(STATE), LIMIT)?)?;
    state.validate()?;
    Ok(Some(state))
}

pub(super) fn pointer(root: &Path, name: &str) -> Result<Option<String>, InstallError> {
    let path = root.join(name);
    if !present(&path)? {
        return Ok(None);
    }
    let digest = String::from_utf8(fsafe::bounded_read(&path, 64)?)
        .map_err(|_| InstallError::Rejected("invalid release pointer"))?;
    if !manifest::digest_name(&digest) {
        return Err(InstallError::Rejected("invalid release pointer"));
    }
    Ok(Some(digest))
}

pub(super) fn verify(
    root: &Path,
    state: &State,
    store: &ArtifactStore,
    policy: &InstallPolicy,
) -> Result<(), InstallError> {
    state.validate()?;
    if read_state(root)?.as_ref() != Some(state) {
        return Err(InstallError::Rejected(
            "release journal changed under owner",
        ));
    }
    verify_pointers(root, state)?;
    // Candidate can change through installation. It must be observed again before a transition.
    for digest in state
        .production
        .iter()
        .map(|p| p.digest.as_str())
        .chain(state.previous_good.iter().map(String::as_str))
    {
        store.verify_installed(digest, policy)?;
    }
    Ok(())
}

pub(super) fn verify_pointers(root: &Path, state: &State) -> Result<(), InstallError> {
    if pointer(root, "active")?.as_deref() != state.production.as_ref().map(|p| p.digest.as_str())
        || pointer(root, "previous-good")? != state.previous_good
    {
        return Err(InstallError::Rejected(
            "release pointers disagree with journal",
        ));
    }
    Ok(())
}

pub(super) fn recover(
    root: &Path,
    store: &ArtifactStore,
    policy: &InstallPolicy,
) -> Result<(), InstallError> {
    if present(&root.join(INTENT))? {
        let intent: Intent =
            serde_json::from_slice(&fsafe::bounded_read(&root.join(INTENT), LIMIT)?)?;
        intent.before.validate()?;
        intent.after.validate()?;
        if !intent.after.follows(&intent.before) {
            return Err(InstallError::Rejected("invalid activation intent sequence"));
        }
        let current = read_state(root)?.ok_or(InstallError::Rejected("intent without journal"))?;
        if current != intent.before && current != intent.after {
            return Err(InstallError::Rejected(
                "activation intent disagrees with journal",
            ));
        }
        if pointer(root, "candidate")?.as_deref()
            != intent.after.candidate.as_ref().map(|c| c.digest.as_str())
        {
            return Err(InstallError::Rejected(
                "activation candidate changed during recovery",
            ));
        }
        // No pointer effect occurs before this intent is durable. Once it exists,
        // complete its exact target, including a crash during an earlier recovery.
        for digest in intent.after.digests() {
            store.verify_installed(digest, policy)?;
        }
        for (name, before, after) in pointer_changes(&intent.before, &intent.after) {
            let actual = pointer(root, name)?;
            if actual.as_deref() != before && actual.as_deref() != after {
                return Err(InstallError::Rejected("unexpected pointer during recovery"));
            }
        }
        clean_temporaries(root)?;
        apply(root, &intent.before, &intent.after)?;
        remove(root, INTENT)?;
    } else {
        // Unpublished scratch files have no authority, even if their bytes are complete.
        clean_temporaries(root)?;
    }
    if read_state(root)?.is_none() {
        let active = pointer(root, "active")?;
        let previous_good = pointer(root, "previous-good")?;
        let state = State {
            production: active.map(|digest| Release {
                phase: if previous_good.as_ref() == Some(&digest) {
                    Phase::PreviousGood
                } else {
                    Phase::Installed
                },
                digest,
            }),
            previous_good,
            candidate: pointer(root, "candidate")?.map(|digest| Release {
                digest,
                phase: Phase::Installed,
            }),
            sequence: 0,
        };
        state.validate()?;
        for digest in state.digests() {
            store.verify_installed(digest, policy)?;
        }
        atomic(root, STATE, &serde_json::to_vec(&state)?)?;
    }
    Ok(())
}

fn pointer_changes<'a>(
    before: &'a State,
    after: &'a State,
) -> [(&'static str, Option<&'a str>, Option<&'a str>); 2] {
    [
        (
            "previous-good",
            before.previous_good.as_deref(),
            after.previous_good.as_deref(),
        ),
        (
            "active",
            before.production.as_ref().map(|p| p.digest.as_str()),
            after.production.as_ref().map(|p| p.digest.as_str()),
        ),
    ]
}

pub(super) fn commit(root: &Path, before: &State, after: &State) -> Result<(), InstallError> {
    atomic(
        root,
        INTENT,
        &serde_json::to_vec(&Intent {
            before: before.clone(),
            after: after.clone(),
        })?,
    )?;
    apply(root, before, after)?;
    remove(root, INTENT)
}

fn apply(root: &Path, before: &State, after: &State) -> Result<(), InstallError> {
    for (name, old, new) in pointer_changes(before, after) {
        if old != new {
            if let Some(digest) = new {
                atomic(root, name, digest.as_bytes())?;
            } else {
                remove(root, name)?;
            }
        }
    }
    atomic(root, STATE, &serde_json::to_vec(after)?)
}

fn atomic(root: &Path, name: &str, bytes: &[u8]) -> Result<(), InstallError> {
    let scratch = root.join(format!("{name}.next"));
    let mut file = fsafe::open(&scratch, true, true)?;
    boundary()?;
    file.write_all(bytes)?;
    boundary()?;
    file.sync_all()?;
    boundary()?;
    fs::rename(scratch, root.join(name))?;
    boundary()?;
    fsafe::sync_dir(root)?;
    boundary()
}

fn remove(root: &Path, name: &str) -> Result<(), InstallError> {
    if present(&root.join(name))? {
        // Validate before removal: never follow or silently erase hostile metadata.
        fsafe::open(&root.join(name), false, false)?;
        fs::remove_file(root.join(name))?;
        boundary()?;
        fsafe::sync_dir(root)?;
        boundary()?;
    }
    Ok(())
}

fn clean_temporaries(root: &Path) -> Result<(), InstallError> {
    for name in [
        ".activation-intent.next",
        ".release-state.json.next",
        "active.next",
        "previous-good.next",
    ] {
        remove(root, name)?;
    }
    Ok(())
}

#[cfg_attr(not(test), allow(clippy::unnecessary_wraps))] // Test-only I/O fault seam.
fn boundary() -> Result<(), InstallError> {
    #[cfg(test)]
    FAULT.with(|fault| -> Result<(), InstallError> {
        let left = fault.get();
        if let Some(left) = left {
            fault.set(left.checked_sub(1));
            if left == 0 {
                return Err(std::io::Error::other("injected interruption").into());
            }
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
pub(super) static FAULT: std::thread::LocalKey<std::cell::Cell<Option<usize>>> = {
    std::thread_local! { static SLOT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
    SLOT
};
