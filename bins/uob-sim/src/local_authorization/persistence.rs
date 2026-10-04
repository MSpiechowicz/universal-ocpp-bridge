use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::PrivateState;
use zeroize::Zeroizing;

const FILE_LIMIT: u64 = 2 * 1024 * 1024;

pub(super) struct PrivateStorage {
    path: PathBuf,
    parent: PathBuf,
    owner: u32,
    lock: File,
    lock_path: PathBuf,
    parent_identity: (u64, u64),
    #[cfg(test)]
    pub(super) fail_directory_sync: bool,
}

impl Drop for PrivateStorage {
    fn drop(&mut self) {
        // A forked helper may retain this open-file description until exec.
        // Release ownership now rather than waiting for its inherited fd to close.
        let _ = self.lock.unlock();
    }
}

impl PrivateStorage {
    pub(super) fn open(path: &str) -> Result<Self, &'static str> {
        let path = PathBuf::from(path);
        let parent = path.parent().ok_or("private_state_path")?.to_path_buf();
        if !path.is_absolute()
            || fs::canonicalize(&parent).map_err(|_| "private_state_directory")? != parent
        {
            return Err("private_state_path");
        }
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "private_state_owner")?
            .uid();
        let metadata = fs::symlink_metadata(&parent).map_err(|_| "private_state_directory")?;
        if !metadata.is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err("private_state_directory_permissions");
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("private_state_path")?;
        if matches!(name, "." | "..") {
            return Err("private_state_path");
        }
        let lock_path = parent.join(format!("{name}.lock"));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|_| "private_state_lock")?;
        check_file(&lock.metadata().map_err(|_| "private_state_lock")?, owner)?;
        lock.try_lock().map_err(|_| "private_state_already_owned")?;
        let storage = Self {
            path,
            parent,
            owner,
            lock,
            lock_path,
            parent_identity: (metadata.dev(), metadata.ino()),
            #[cfg(test)]
            fail_directory_sync: false,
        };
        storage.check_existing()?;
        Ok(storage)
    }

    fn check_existing(&self) -> Result<(), &'static str> {
        let parent = fs::symlink_metadata(&self.parent).map_err(|_| "private_state_directory")?;
        if !parent.is_dir()
            || parent.uid() != self.owner
            || parent.permissions().mode() & 0o777 != 0o700
            || (parent.dev(), parent.ino()) != self.parent_identity
            || fs::canonicalize(&self.parent).map_err(|_| "private_state_directory")? != self.parent
        {
            return Err("private_state_directory_permissions");
        }
        let held = self.lock.metadata().map_err(|_| "private_state_lock")?;
        let current = fs::symlink_metadata(&self.lock_path).map_err(|_| "private_state_lock")?;
        check_file(&current, self.owner)?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err("private_state_lock_replaced");
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) => {
                check_file(&metadata, self.owner)?;
                if fs::canonicalize(&self.path).map_err(|_| "private_state_path")? != self.path {
                    return Err("private_state_path");
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("private_state_unreadable"),
        }
    }

    pub(super) fn load(&self, station: &str) -> Result<Option<PrivateState>, &'static str> {
        self.check_existing()?;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("private_state_unreadable"),
        };
        let metadata = file.metadata().map_err(|_| "private_state_unreadable")?;
        check_file(&metadata, self.owner)?;
        if metadata.len() > FILE_LIMIT {
            return Err("private_state_capacity");
        }
        let mut bytes = Zeroizing::new(Vec::new());
        (&file)
            .take(FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "private_state_unreadable")?;
        let parsed = if bytes.len() as u64 > FILE_LIMIT {
            Err("private_state_capacity")
        } else {
            serde_json::from_slice::<PrivateState>(&bytes).map_err(|_| "private_state_corrupt")
        };
        let mut state = parsed?;
        if state.format_version != 1 {
            return Err("private_state_version");
        }
        if state.station != station || state.protocol != "ocpp1.6" {
            return Err("private_state_binding");
        }
        // A process may have died after a native send but before persisting its response.
        for record in &mut state.offline {
            record.replay.recover_uncertain();
        }
        file.sync_all().map_err(|_| "private_state_recovery_sync")?;
        File::open(&self.parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "private_state_recovery_sync")?;
        Ok(Some(state))
    }

    pub(super) fn commit(&self, state: &PrivateState) -> Result<(), &'static str> {
        self.check_existing()?;
        let temp = self
            .parent
            .join(format!(".uob-local-{}.tmp", uuid::Uuid::new_v4()));
        let result = self.write_replace(state, &temp);
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn write_replace(&self, state: &PrivateState, temp: &Path) -> Result<(), &'static str> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(temp)
            .map_err(|_| "private_state_write")?;
        let mut writer = BoundedWriter {
            file,
            remaining: FILE_LIMIT,
        };
        serde_json::to_writer(&mut writer, state).map_err(|_| "private_state_write")?;
        writer.file.sync_all().map_err(|_| "private_state_sync")?;
        fs::rename(temp, &self.path).map_err(|_| "private_state_replace")?;
        #[cfg(test)]
        if self.fail_directory_sync {
            return Err("private_state_commit_uncertain");
        }
        File::open(&self.parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "private_state_commit_uncertain")?;
        Ok(())
    }
}

fn check_file(metadata: &fs::Metadata, owner: u32) -> Result<(), &'static str> {
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        Err("private_state_file_permissions")
    } else {
        Ok(())
    }
}

struct BoundedWriter {
    file: File,
    remaining: u64,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(std::io::Error::other("private state capacity"));
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
#[test]
fn last_owner_drop_releases_lock_with_inherited_descriptor() {
    use super::{LocalAuthorizationConfig, LocalAuthorizationHandle};

    let directory = std::env::temp_dir().join(format!("uob-lock-owner-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let config = LocalAuthorizationConfig {
        private_state_file: directory.join("state.json").to_str().unwrap().to_owned(),
        list_supported: true,
        cache_supported: true,
    };
    let owner = LocalAuthorizationHandle::open("lock-owner", &config).unwrap();
    let inherited = owner
        .0
        .lock()
        .storage
        .as_ref()
        .unwrap()
        .lock
        .try_clone()
        .unwrap();
    let remaining_owner = owner.clone();
    drop(owner);
    assert!(matches!(
        LocalAuthorizationHandle::open("lock-owner", &config),
        Err("private_state_already_owned")
    ));
    drop(remaining_owner);
    let recovered = LocalAuthorizationHandle::open("lock-owner", &config)
        .expect("the last model owner must release the lock despite an inherited descriptor");
    drop(inherited);
    assert!(matches!(
        LocalAuthorizationHandle::open("lock-owner", &config),
        Err("private_state_already_owned")
    ));
    drop(recovered);
    fs::remove_dir_all(directory).unwrap();
}
