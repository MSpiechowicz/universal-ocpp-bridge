use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

const FILE_LIMIT: u64 = 2 * 1024 * 1024;

/// Exclusive owner-only durable file shared by the independent native reservation models.
pub(crate) struct PrivateStorage {
    path: PathBuf,
    parent: PathBuf,
    owner: u32,
    lock: File,
    lock_path: PathBuf,
    parent_identity: (u64, u64),
    #[cfg(test)]
    pub(crate) fail_directory_sync: bool,
}

impl Drop for PrivateStorage {
    fn drop(&mut self) {
        // A forked helper may retain this open-file description until exec.
        // Release ownership now rather than waiting for its inherited fd to close.
        let _ = self.lock.unlock();
    }
}

impl PrivateStorage {
    pub(crate) fn open(path: &str) -> Result<Self, &'static str> {
        let path = PathBuf::from(path);
        let parent = path.parent().ok_or("reservation_state_path")?.to_path_buf();
        if !path.is_absolute()
            || fs::canonicalize(&parent).map_err(|_| "reservation_state_directory")? != parent
        {
            return Err("reservation_state_path");
        }
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "reservation_state_owner")?
            .uid();
        let metadata = fs::symlink_metadata(&parent).map_err(|_| "reservation_state_directory")?;
        if !metadata.is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err("reservation_state_directory_permissions");
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("reservation_state_path")?;
        if matches!(name, "." | "..") {
            return Err("reservation_state_path");
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
            .map_err(|_| "reservation_state_lock")?;
        check_file(
            &lock.metadata().map_err(|_| "reservation_state_lock")?,
            owner,
        )?;
        lock.try_lock()
            .map_err(|_| "reservation_state_already_owned")?;
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
        let parent =
            fs::symlink_metadata(&self.parent).map_err(|_| "reservation_state_directory")?;
        if !parent.is_dir()
            || parent.uid() != self.owner
            || parent.permissions().mode() & 0o777 != 0o700
            || (parent.dev(), parent.ino()) != self.parent_identity
            || fs::canonicalize(&self.parent).map_err(|_| "reservation_state_directory")?
                != self.parent
        {
            return Err("reservation_state_directory_permissions");
        }
        let held = self.lock.metadata().map_err(|_| "reservation_state_lock")?;
        let current =
            fs::symlink_metadata(&self.lock_path).map_err(|_| "reservation_state_lock")?;
        check_file(&current, self.owner)?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err("reservation_state_lock_replaced");
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) => {
                check_file(&metadata, self.owner)?;
                if fs::canonicalize(&self.path).map_err(|_| "reservation_state_path")? != self.path
                {
                    return Err("reservation_state_path");
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("reservation_state_unreadable"),
        }
    }

    pub(crate) fn load<T: DeserializeOwned>(&self) -> Result<Option<T>, &'static str> {
        self.check_existing()?;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("reservation_state_unreadable"),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "reservation_state_unreadable")?;
        check_file(&metadata, self.owner)?;
        if metadata.len() > FILE_LIMIT {
            return Err("reservation_state_capacity");
        }
        let mut bytes = Zeroizing::new(Vec::new());
        (&file)
            .take(FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "reservation_state_unreadable")?;
        let parsed = if bytes.len() as u64 > FILE_LIMIT {
            Err("reservation_state_capacity")
        } else {
            serde_json::from_slice::<T>(&bytes).map_err(|_| "reservation_state_corrupt")
        };
        let state = parsed?;
        file.sync_all()
            .map_err(|_| "reservation_state_recovery_sync")?;
        File::open(&self.parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "reservation_state_recovery_sync")?;
        Ok(Some(state))
    }

    pub(crate) fn commit<T: Serialize>(&self, state: &T) -> Result<(), &'static str> {
        self.check_existing()?;
        let temp = self
            .parent
            .join(format!(".uob-reservation-{}.tmp", uuid::Uuid::new_v4()));
        let result = self.write_replace(state, &temp);
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn write_replace<T: Serialize>(&self, state: &T, temp: &Path) -> Result<(), &'static str> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(temp)
            .map_err(|_| "reservation_state_write")?;
        let mut writer = BoundedWriter {
            file,
            remaining: FILE_LIMIT,
        };
        serde_json::to_writer(&mut writer, state).map_err(|_| "reservation_state_write")?;
        writer
            .file
            .sync_all()
            .map_err(|_| "reservation_state_sync")?;
        fs::rename(temp, &self.path).map_err(|_| "reservation_state_replace")?;
        #[cfg(test)]
        if self.fail_directory_sync {
            return Err("reservation_commit_uncertain");
        }
        File::open(&self.parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "reservation_commit_uncertain")?;
        Ok(())
    }
}

fn check_file(metadata: &fs::Metadata, owner: u32) -> Result<(), &'static str> {
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        Err("reservation_state_file_permissions")
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
