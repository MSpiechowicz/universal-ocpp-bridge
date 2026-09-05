//! Host-owned deployment identity, private state layout, and exclusive process ownership.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use uob_contracts::{Environment, ServiceIdentity};
use uob_storage_adapter::{DEFAULT_WORK_QUEUE_CAPACITY, SqliteOperationalStore};

const INVALID: &str = "deployment identity or private filesystem layout invalid";

pub(crate) struct DeploymentLayout {
    state: PathBuf,
    runtime: PathBuf,
}

impl DeploymentLayout {
    pub(crate) fn from_environment(environment: Environment) -> Result<Option<Self>, &'static str> {
        let Some(expected) = std::env::var_os("UOB_DEPLOYMENT_ENVIRONMENT") else {
            return Ok(None);
        };
        let expected = match expected.to_str() {
            Some("production") => Environment::Production,
            Some("staging") => Environment::Staging,
            _ => return Err(INVALID),
        };
        if expected != environment {
            return Err(INVALID);
        }
        let state = std::env::var_os("STATE_DIRECTORY").ok_or(INVALID)?.into();
        let runtime = std::env::var_os("RUNTIME_DIRECTORY").ok_or(INVALID)?.into();
        Self::new(state, runtime).map(Some)
    }

    fn new(state: PathBuf, runtime: PathBuf) -> Result<Self, &'static str> {
        for path in [&state, &runtime] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
                || path.as_os_str().to_string_lossy().contains(':')
            {
                return Err(INVALID);
            }
        }
        if state.starts_with(&runtime) || runtime.starts_with(&state) {
            return Err(INVALID);
        }
        Ok(Self { state, runtime })
    }

    pub(crate) fn open(&self, identity: &ServiceIdentity) -> Result<DeploymentState, &'static str> {
        private_directory(&self.state)?;
        private_directory(&self.runtime)?;
        // Never unlink lock files: an old inode could still be locked by another process.
        let state_lock = lock(&self.state.join("service.lock"))?;
        let runtime_lock = lock(&self.runtime.join("service.lock"))?;
        for name in [
            "operational.sqlite3",
            "operational.sqlite3-wal",
            "operational.sqlite3-shm",
        ] {
            regular_file_if_present(&self.state.join(name))?;
        }
        let marker = self.state.join("identity.json");
        regular_file_if_present(&marker)?;
        let expected = DurableIdentity {
            bridge_id: identity.bridge_id.as_str().to_owned(),
            environment: identity.runtime.environment,
        };
        match fs::read(&marker) {
            Ok(bytes) => {
                let stored: DurableIdentity =
                    serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
                if stored != expected {
                    return Err(INVALID);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An existing unbound database needs an explicit operator migration.
                if self.state.join("operational.sqlite3").exists() {
                    return Err(INVALID);
                }
                let bytes = serde_json::to_vec(&expected).map_err(|_| INVALID)?;
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker)
                    .map_err(|_| INVALID)?;
                file.write_all(&bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| INVALID)?;
                File::open(&self.state)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| INVALID)?;
            }
            Err(_) => return Err(INVALID),
        }
        let store = SqliteOperationalStore::open(
            self.state.join("operational.sqlite3"),
            DEFAULT_WORK_QUEUE_CAPACITY,
        )
        .map_err(|_| "deployment operational storage unavailable")?;
        Ok(DeploymentState {
            store,
            _state_lock: state_lock,
            _runtime_lock: runtime_lock,
        })
    }
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct DurableIdentity {
    bridge_id: String,
    environment: Environment,
}

pub(crate) struct DeploymentState {
    // Charging composition will use this same store; never open a second authoritative path.
    store: SqliteOperationalStore<(), (), (), ()>,
    _state_lock: File,
    _runtime_lock: File,
}

impl DeploymentState {
    pub(crate) async fn shutdown(&self, deadline: Duration) -> Result<(), &'static str> {
        self.store
            .shutdown(deadline)
            .await
            .map_err(|_| "deployment storage shutdown failed")
    }
}

fn lock(path: &Path) -> Result<File, &'static str> {
    regular_file_if_present(path)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| INVALID)?;
    file.try_lock()
        .map_err(|_| "deployment state already owned or lock unavailable")?;
    Ok(file)
}

fn private_directory(path: &Path) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| INVALID)?;
    if !metadata.is_dir() || fs::canonicalize(path).map_err(|_| INVALID)? != path {
        return Err(INVALID);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn regular_file_if_present(path: &Path) -> Result<(), &'static str> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(INVALID);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(INVALID);
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(INVALID),
    }
}

#[cfg(test)]
mod tests;
