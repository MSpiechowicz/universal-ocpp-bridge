use std::{cmp::Ordering, time::Duration};

use rusqlite::Connection;
use uob_application::{StorageError, StorageErrorCode};

use crate::schema;

/// Minimum bundled `SQLite` release containing the required WAL-reset correction.
pub const MINIMUM_SQLITE_VERSION: &str = "3.51.3";

/// Verified settings of the connection serving authoritative writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteRuntimeConfiguration {
    /// `SQLite` library linked into the storage adapter.
    pub sqlite_version: String,
    /// Effective journal mode; always `wal` after a successful open.
    pub journal_mode: String,
    /// Effective synchronous level; always `2` (`FULL`) after a successful open.
    pub synchronous: i64,
}

pub(crate) fn configure(
    connection: &Connection,
) -> Result<SqliteRuntimeConfiguration, StorageError> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(unavailable)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;\n\
             PRAGMA journal_mode = WAL;\n\
             PRAGMA synchronous = FULL;",
        )
        .map_err(unavailable)?;
    schema::migrate(connection)?;

    let sqlite_version = rusqlite::version().to_owned();
    if compare_versions(&sqlite_version, MINIMUM_SQLITE_VERSION) == Ordering::Less {
        return Err(StorageError::new(
            StorageErrorCode::Unavailable,
            format!(
                "bundled SQLite {sqlite_version} is older than required {MINIMUM_SQLITE_VERSION}"
            ),
        ));
    }
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(unavailable)?;
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(unavailable)?;
    if !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 2 {
        return Err(StorageError::new(
            StorageErrorCode::Unavailable,
            "SQLite did not retain required WAL and synchronous=FULL settings",
        ));
    }
    Ok(SqliteRuntimeConfiguration {
        sqlite_version,
        journal_mode,
        synchronous,
    })
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let left = parse(left);
    let right = parse(right);
    (0..left.len().max(right.len()))
        .map(|index| {
            left.get(index)
                .copied()
                .unwrap_or(0)
                .cmp(&right.get(index).copied().unwrap_or(0))
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

pub(crate) fn unavailable(error: rusqlite::Error) -> StorageError {
    let code = match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
                || failure.code == rusqlite::ErrorCode::DatabaseLocked =>
        {
            StorageErrorCode::Busy
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StorageErrorCode::Conflict
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseCorrupt =>
        {
            StorageErrorCode::IntegrityFailure
        }
        _ => StorageErrorCode::Unavailable,
    };
    drop(error);
    StorageError::new(code, "operational SQLite request failed")
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::compare_versions;

    #[test]
    fn compares_numeric_version_components() {
        assert_eq!(compare_versions("3.51.3", "3.51.3"), Ordering::Equal);
        assert_eq!(compare_versions("3.51.10", "3.51.3"), Ordering::Greater);
        assert_eq!(compare_versions("3.50.9", "3.51.3"), Ordering::Less);
    }
}
