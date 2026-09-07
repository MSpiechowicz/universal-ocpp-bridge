use super::{InstallError, reject};
use rustix::fs::{FallocateFlags, OFlags, fallocate, statvfs};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub const HEADROOM: u64 = 512 * 1024 * 1024;

pub fn directory(path: &Path) -> Result<(), InstallError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o022 != 0
        || fs::canonicalize(path)? != path
    {
        return reject("store path must be canonical, owner-controlled and not writable by peers");
    }
    Ok(())
}

pub fn open(path: &Path, write: bool, create: bool) -> Result<File, InstallError> {
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .create_new(create)
        .mode(0o600)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed())
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o022 != 0
    {
        return reject("store file must be an owner-controlled regular file without hardlinks");
    }
    Ok(file)
}

pub fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, InstallError> {
    let file = open(path, false, false)?;
    let mut result = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut result)?;
    if result.len() > limit {
        return reject("metadata exceeds size bound");
    }
    Ok(result)
}

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    let mut file = open(path, true, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn sync_dir(path: &Path) -> Result<(), InstallError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub fn capacity(path: &Path, bytes: u64, inodes: u64) -> Result<(), InstallError> {
    let stat = statvfs(path)?;
    if stat.f_bavail.saturating_mul(stat.f_frsize) < HEADROOM.saturating_add(bytes)
        || stat.f_favail < inodes.saturating_add(16)
    {
        return reject("insufficient disk space or inodes; retained artifacts must not be removed");
    }
    Ok(())
}

pub fn allocate(file: &File, bytes: u64) -> Result<(), InstallError> {
    if bytes != 0 {
        fallocate(file, FallocateFlags::empty(), 0, bytes)?;
    }
    Ok(())
}

pub fn copy_exact(
    reader: &mut impl Read,
    file: &mut File,
    size: u64,
    digest: &mut Sha256,
) -> Result<(), InstallError> {
    let mut buffer = vec![0; 64 * 1024];
    let mut remaining = size;
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded chunk");
        reader.read_exact(&mut buffer[..count])?;
        digest.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    file.sync_all()?;
    Ok(())
}

// Used only beneath the private, installer-owned store under its exclusive lock.
// Symlinks are rejected, never followed. Published directories need temporary owner
// write permission for unlinking their contents; active/previous are excluded by caller.
pub fn remove_tree(path: &Path) -> Result<(), InstallError> {
    directory(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            remove_tree(&path)?;
        } else if meta.is_file() && meta.nlink() == 1 {
            fs::remove_file(path)?;
        } else {
            return reject("unexpected entry in private artifact directory");
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

pub fn seal(path: &Path) -> Result<(), InstallError> {
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        if fs::symlink_metadata(&child)?.is_dir() {
            seal(&child)?;
        }
    }
    fs::set_permissions(path, Permissions::from_mode(0o555))?;
    sync_dir(path)
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut output, b| {
            write!(output, "{b:02x}").expect("write to string");
            output
        })
}

/// Enumerates only the bounded signed layout, rejecting even unsigned extra assets.
pub fn verify_tree(
    root: &Path,
    path: &Path,
    expected: &std::collections::BTreeSet<String>,
) -> Result<(), InstallError> {
    directory(path)?;
    if fs::metadata(path)?.mode() & 0o777 != 0o555 {
        return reject("artifact directory is not sealed");
    }
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let relative = child
            .strip_prefix(root)
            .expect("artifact descendant")
            .to_str()
            .ok_or(InstallError::Rejected("non-UTF8 artifact path"))?;
        let meta = fs::symlink_metadata(&child)?;
        if meta.is_dir() {
            let prefix = format!("{relative}/");
            if !expected.iter().any(|file| file.starts_with(&prefix)) {
                return reject("unsigned artifact directory");
            }
            verify_tree(root, &child, expected)?;
        } else {
            if !expected.contains(relative) || !meta.is_file() || meta.nlink() != 1 {
                return reject("unsigned file or link in artifact");
            }
            let mode = if relative == "bin/uob" { 0o555 } else { 0o444 };
            if meta.mode() & 0o7777 != mode {
                return reject("artifact file permissions changed");
            }
        }
    }
    Ok(())
}
