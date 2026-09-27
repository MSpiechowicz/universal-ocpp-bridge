use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use rusqlite::Connection;
use uob_application::{
    EXPORT_SPOOL_ENVELOPE_BYTES, ExportSpoolError, ExportSpoolErrorCode, RuntimeResourceBudget,
};

use super::{Limits, fail, schema};

pub(super) fn open_database(
    directory: &Path,
    operational: &Path,
    limits: Limits,
    budget: &RuntimeResourceBudget,
) -> Result<Connection, ExportSpoolError> {
    if !directory.is_absolute() || !operational.is_absolute() {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "spool and operational paths must be absolute",
        ));
    }
    let canonical = fs::canonicalize(directory).map_err(io_error)?;
    if canonical != directory {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "spool directory must not use a symlink or path alias",
        ));
    }
    let meta = fs::symlink_metadata(directory).map_err(io_error)?;
    let operational_dir = operational.parent().ok_or_else(|| {
        ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "invalid operational database path",
        )
    })?;
    let operational_dev = fs::metadata(operational_dir).map_err(io_error)?.dev();
    if !meta.is_dir() || meta.permissions().mode() & 0o777 != 0o700 || meta.dev() == operational_dev
    {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::InvalidRequest,
            "export spool requires a private, separate mounted storage device",
        ));
    }
    match fs::metadata(operational) {
        Ok(operational_file) if operational_file.dev() == meta.dev() => {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::InvalidRequest,
                "operational database must not reside on the export spool device",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }

    let database = directory.join("export.sqlite3");
    let journal_limit = limits.main_bytes + limits.main_bytes / 128 + 1024 * 1024;
    let mut occupied = 0_u64;
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let journal = entry.file_name() == "export.sqlite3-journal";
        if entry.file_name() != "export.sqlite3" && !journal {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::IntegrityFailure,
                "unexpected spool sidecar or file in isolated directory",
            ));
        }
        let file = fs::symlink_metadata(entry.path()).map_err(io_error)?;
        if !file.is_file()
            || file.nlink() != 1
            || file.uid() != meta.uid()
            || file.permissions().mode() & 0o777 != 0o600
        {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::IntegrityFailure,
                "spool database and recovery journal must be private regular files",
            ));
        }
        if journal && file.len() > journal_limit {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::Backpressure,
                "spool recovery journal exceeds its physical envelope",
            ));
        }
        occupied = occupied.saturating_add(file.blocks().saturating_mul(512));
    }
    if occupied > EXPORT_SPOOL_ENVELOPE_BYTES {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::Backpressure,
            "spool files exceed their total physical envelope",
        ));
    }
    if !database.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&database)
            .map_err(io_error)?;
    }
    let connection = Connection::open(&database).map_err(|error| fail(&error))?;
    schema::configure(&connection, limits, budget)?;
    let length = fs::metadata(&database).map_err(io_error)?.len();
    if length > limits.main_bytes || length > EXPORT_SPOOL_ENVELOPE_BYTES {
        return Err(ExportSpoolError::new(
            ExportSpoolErrorCode::Backpressure,
            "spool main file exceeds the physical envelope",
        ));
    }
    Ok(connection)
}

fn io_error(_: std::io::Error) -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::Unavailable,
        "isolated export spool filesystem unavailable",
    )
}
