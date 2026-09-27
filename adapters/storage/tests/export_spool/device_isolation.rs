use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use uob_application::ExportSpoolErrorCode;
use uob_storage_adapter::{ExportSpoolLimits, SqliteExportSpool};

use super::support::Fixture;

#[test]
fn operational_database_symlink_into_spool_device_is_rejected() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let database = fixture.directory.join("export.sqlite3");
    fs::write(&database, []).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&database, &fixture.source).unwrap();

    let result = SqliteExportSpool::open_with_limits(
        &fixture.directory,
        &fixture.source,
        8,
        ExportSpoolLimits::new(4 * 1024 * 1024, 8).unwrap(),
        &fixture.budget,
    );
    fs::remove_file(&fixture.source).unwrap();

    let error = result
        .err()
        .expect("symlinked operational database was accepted");
    assert_eq!(error.code(), ExportSpoolErrorCode::InvalidRequest);
    assert_eq!(fs::metadata(&database).unwrap().len(), 0);
    fs::write(&fixture.source, []).unwrap();

    let spool = fixture.open(4 * 1024 * 1024, 8);
    drop(spool);
    fs::remove_file(&fixture.source).unwrap();
}
