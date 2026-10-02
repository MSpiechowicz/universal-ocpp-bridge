use super::{CAPACITY, FRAME_LIMIT, validation};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

const STATE_LIMIT: u64 = (CAPACITY * FRAME_LIMIT + 1024) as u64;

fn private(metadata: &fs::Metadata, directory: bool) -> io::Result<()> {
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(io::Error::other(
            "independent state must be private and owned",
        ));
    }
    Ok(())
}

fn parent(path: &Path) -> io::Result<&Path> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other("independent state requires parent directory"))?;
    private(&fs::symlink_metadata(parent)?, true)?;
    Ok(parent)
}

pub(super) fn load(path: &Path) -> io::Result<Vec<Value>> {
    parent(path)?;
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error),
    };
    private(&file.metadata()?, false)?;
    if file.metadata()?.len() > STATE_LIMIT {
        return Err(io::Error::other("independent state exceeds bound"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(STATE_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > STATE_LIMIT {
        return Err(io::Error::other("independent state exceeds bound"));
    }
    let profiles: Vec<Value> = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::other("invalid independent state JSON"))?;
    if profiles.len() > CAPACITY
        || profiles.iter().any(|p| !validation::structure(p))
        || profiles
            .iter()
            .enumerate()
            .any(|(i, p)| profiles[..i].iter().any(|old| validation::same_id(old, p)))
    {
        return Err(io::Error::other("invalid independent profile inventory"));
    }
    Ok(profiles)
}

pub(super) fn persist(path: &Path, profiles: &[Value]) -> io::Result<()> {
    let directory = parent(path)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        private(&metadata, false)?;
    }
    let bytes = serde_json::to_vec(profiles)
        .map_err(|_| io::Error::other("independent state encoding failed"))?;
    if profiles.len() > CAPACITY || bytes.len() as u64 > STATE_LIMIT {
        return Err(io::Error::other("independent state exceeds bound"));
    }
    let temporary = directory.join(format!(".profile201-{}.new", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
