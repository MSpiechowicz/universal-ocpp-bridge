//! Bounded online disaster-recovery backups. Never used to select rollback state.
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};

/// Hard resource limits, including verification work.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub maximum_bytes: u64,
    pub expected_schema_version: u32,
    pub timeout: Duration,
}

/// Sanitized failure; SQL and filesystem details must not escape to diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackupFailed;

/// Creates a new private backup using SQLite's online backup API, including committed WAL.
/// The caller owns a private destination directory and must serialize backup operations.
/// No existing destination is overwritten. On failure an incomplete file is removed.
///
/// # Errors
/// Rejects invalid limits, unsafe files, oversized databases, contention, timeout,
/// corruption and failed verification. Does not migrate or restore either database.
pub fn create(source: &Path, destination: &Path, limits: Limits) -> Result<u64, BackupFailed> {
    if limits.maximum_bytes == 0
        || limits.maximum_bytes > 64 * 1024 * 1024 * 1024
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(300)
        || source == destination
    {
        return Err(BackupFailed);
    }
    let meta = fs::symlink_metadata(source).map_err(|_| BackupFailed)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || fs::canonicalize(source).map_err(|_| BackupFailed)? != source
    {
        return Err(BackupFailed);
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|_| BackupFailed)?;
    let result = copy(source, destination, limits).and_then(|bytes| {
        file.sync_all().map_err(|_| BackupFailed)?;
        Ok(bytes)
    });
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

fn copy(source: &Path, destination: &Path, limits: Limits) -> Result<u64, BackupFailed> {
    let deadline = Instant::now() + limits.timeout;
    let work = || -> rusqlite::Result<u64> {
        let input = Connection::open_with_flags(
            source,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        input.busy_timeout(Duration::ZERO)?;
        input.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        // Pin one read snapshot so concurrent writers cannot restart the copy forever.
        input.execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")?;
        let schema: u32 = input.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if schema != limits.expected_schema_version {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let pages: u32 = input.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: u32 = input.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let bytes = u64::from(pages)
            .checked_mul(u64::from(page_size))
            .filter(|n| *n > 0 && *n <= limits.maximum_bytes)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let mut output = Connection::open_with_flags(
            destination,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        output.busy_timeout(Duration::ZERO)?;
        output.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=-1024;",
        )?;
        {
            let backup = Backup::new(&input, &mut output)?;
            loop {
                if Instant::now() >= deadline {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                match backup.step(64)? {
                    StepResult::Done => break,
                    StepResult::More => std::thread::yield_now(),
                    _ => return Err(rusqlite::Error::InvalidQuery),
                }
            }
        }
        input.execute_batch("ROLLBACK")?;
        output.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        let integrity: String = output.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
        if integrity != "ok" || Instant::now() >= deadline {
            return Err(rusqlite::Error::InvalidQuery);
        }
        output.close().map_err(|(_, e)| e)?;
        Ok(bytes)
    };
    work().map_err(|_| BackupFailed)
}

/// Validate current committed data in place without backup, migration or restoration.
/// The caller must first stop all production writers and retain exclusive process ownership.
///
/// # Errors
/// Rejects unsafe paths, schema mismatch, corruption, capacity excess and deadline expiry.
pub fn validate_current(source: &Path, limits: Limits) -> Result<(), BackupFailed> {
    if limits.maximum_bytes == 0
        || limits.maximum_bytes > 64 * 1024 * 1024 * 1024
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(300)
    {
        return Err(BackupFailed);
    }
    let meta = fs::symlink_metadata(source).map_err(|_| BackupFailed)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || fs::canonicalize(source).map_err(|_| BackupFailed)? != source
    {
        return Err(BackupFailed);
    }
    let deadline = Instant::now() + limits.timeout;
    let check = || -> rusqlite::Result<()> {
        let db = Connection::open_with_flags(
            source,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        db.execute_batch("PRAGMA cache_size=-1024; BEGIN;")?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let pages: u32 = db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let size: u32 = db.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        if version != limits.expected_schema_version
            || pages == 0
            || u64::from(pages) * u64::from(size) > limits.maximum_bytes
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let integrity: String = db.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
        if integrity != "ok" || Instant::now() >= deadline {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    };
    check().map_err(|_| BackupFailed)
}

#[cfg(test)]
mod tests;
