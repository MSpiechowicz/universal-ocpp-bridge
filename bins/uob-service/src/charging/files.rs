use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use rustix::fs::OFlags;

const INVALID: &str = "charging private state or credential unavailable";
const NOFOLLOW: i32 = OFlags::NOFOLLOW.bits().cast_signed();

pub(crate) struct ReadGrant(Vec<u8>);

impl ReadGrant {
    pub(crate) fn token(&self) -> Vec<u8> {
        self.0.clone()
    }
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for ReadGrant {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

pub(super) fn directory(path: &Path) -> Result<File, &'static str> {
    let meta = fs::symlink_metadata(path).map_err(|_| INVALID)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.permissions().mode() & 0o777 != 0o700
        || fs::canonicalize(path).map_err(|_| INVALID)? != path
    {
        return Err(INVALID);
    }
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)
        .map_err(|_| INVALID)?;
    let opened = dir.metadata().map_err(|_| INVALID)?;
    if !opened.is_dir() || opened.dev() != meta.dev() || opened.ino() != meta.ino() {
        return Err(INVALID);
    }
    Ok(dir)
}

fn opened_file(path: &Path, max: u64) -> Result<File, &'static str> {
    if fs::canonicalize(path).map_err(|_| INVALID)? != path {
        return Err(INVALID);
    }
    let before = fs::symlink_metadata(path).map_err(|_| INVALID)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)
        .map_err(|_| INVALID)?;
    let meta = file.metadata().map_err(|_| INVALID)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.permissions().mode() & 0o777 != 0o600
        || meta.len() == 0
        || meta.len() > max
        || before.dev() != meta.dev()
        || before.ino() != meta.ino()
    {
        return Err(INVALID);
    }
    Ok(file)
}

pub(super) fn secret(
    path: &Path,
    seen: &mut BTreeSet<(u64, u64)>,
) -> Result<Vec<u8>, &'static str> {
    let file = opened_file(path, 4096)?;
    let meta = file.metadata().map_err(|_| INVALID)?;
    if !seen.insert((meta.dev(), meta.ino())) {
        return Err(INVALID);
    }
    let expected_len = usize::try_from(meta.len()).map_err(|_| INVALID)?;
    let mut bytes = Vec::with_capacity(expected_len);
    if file.take(4097).read_to_end(&mut bytes).is_err()
        || bytes.len() < 16
        || bytes.len() > 4096
        || bytes.len() != expected_len
    {
        bytes.fill(0);
        return Err(INVALID);
    }
    Ok(bytes)
}
pub(super) fn identity(path: &Path) -> Result<Vec<u8>, &'static str> {
    let file = opened_file(path, 4096)?;
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    Ok(bytes)
}

pub(super) fn grant(bytes: Vec<u8>) -> ReadGrant {
    ReadGrant(bytes)
}

pub(super) fn state_lock(dir: &Path) -> Result<File, &'static str> {
    let path = dir.join("charging.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(NOFOLLOW)
        .open(&path)
        .map_err(|_| INVALID)?;
    let meta = file.metadata().map_err(|_| INVALID)?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.nlink() != 1
        || meta.permissions().mode() & 0o777 != 0o600
    {
        return Err(INVALID);
    }
    file.try_lock()
        .map_err(|_| "charging state already owned")?;
    Ok(file)
}

pub(super) fn check_database_files(dir: &Path) -> Result<(), &'static str> {
    for name in [
        "charging.sqlite3",
        "charging.sqlite3-wal",
        "charging.sqlite3-shm",
    ] {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(meta)
                if meta.is_file()
                    && meta.nlink() == 1
                    && meta.uid() == rustix::process::geteuid().as_raw()
                    && meta.permissions().mode() & 0o777 == 0o600 => {}
            _ => return Err(INVALID),
        }
    }
    Ok(())
}
