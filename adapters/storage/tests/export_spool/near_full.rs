use rusqlite::{Connection, params};
use std::{fs, os::unix::fs::PermissionsExt};
use uob_application::{Durability, ExportSpool};

use super::support::{Fixture, checkpoint, namespace};

#[tokio::test]
async fn near_full_mixed_legacy_rows_migrate_without_evicting_critical() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let database = fixture.directory.join("export.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=DELETE;
        CREATE TABLE binding (id INTEGER PRIMARY KEY CHECK(id=1), destination TEXT NOT NULL,
        revision INTEGER NOT NULL, kind TEXT NOT NULL, generation TEXT NOT NULL,
        critical_cursor TEXT, critical_seq INTEGER NOT NULL DEFAULT 0,
        telemetry_cursor TEXT, telemetry_seq INTEGER NOT NULL DEFAULT 0,
        critical_high INTEGER NOT NULL DEFAULT 0, telemetry_high INTEGER NOT NULL DEFAULT 0,
        incomplete INTEGER NOT NULL DEFAULT 0, legacy INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE pending (durability INTEGER NOT NULL, sequence INTEGER NOT NULL,
        payload BLOB NOT NULL, PRIMARY KEY(durability,sequence)) WITHOUT ROWID;
        CREATE TABLE gaps (durability INTEGER NOT NULL, first INTEGER NOT NULL, last INTEGER NOT NULL,
        reason INTEGER NOT NULL, CHECK(first > 0 AND last >= first), PRIMARY KEY(durability,first)) WITHOUT ROWID;
        INSERT INTO binding(id,destination,revision,kind,generation)
        VALUES(1,'analytics',1,'postgresql','source-test');
        PRAGMA user_version=1; BEGIN IMMEDIATE;").unwrap();
    let large = format!(
        "{{\"record_id\":\"large\",\"committed_at\":\"at\",\"record\":\"{}\"}}",
        "L".repeat(900 * 1024)
    );
    let small = format!(
        "{{\"record_id\":\"small\",\"committed_at\":\"at\",\"record\":\"{}\"}}",
        "s".repeat(4 * 1024)
    );
    let large_count = 49_i64;
    let small_count = 2_500_i64;
    for sequence in 1..=large_count + small_count {
        let bytes = if sequence <= large_count {
            large.as_bytes()
        } else {
            small.as_bytes()
        };
        connection
            .execute(
                "INSERT INTO pending VALUES(0,?1,?2)",
                params![sequence, bytes],
            )
            .unwrap();
    }
    let total = u64::try_from(large_count + small_count).expect("positive fixture sequence");
    connection
        .execute(
            "UPDATE binding SET critical_cursor=?1, critical_seq=?2, critical_high=?2 WHERE id=1",
            params![
                checkpoint(Durability::Critical, total).cursor.as_str(),
                large_count + small_count
            ],
        )
        .unwrap();
    connection.execute_batch("COMMIT").unwrap();
    let pages: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    let original = u64::try_from(pages).expect("nonnegative SQLite page count") * 4096;
    assert!(
        (53 * 1024 * 1024..=57 * 1024 * 1024).contains(&original),
        "v1 fixture not near full: {original}"
    );
    drop(connection);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();

    let spool = fixture.open(60 * 1024 * 1024, 8);
    let recovered = spool.status(namespace()).await.unwrap();
    assert_eq!(recovered.pending_records, total);
    assert_eq!(
        recovered.critical.unwrap().sequence,
        recovered.pending_records
    );
    assert!(recovered.gaps.is_empty());
    let migrated = std::fs::metadata(&database).unwrap().len();
    assert!(migrated <= 60 * 1024 * 1024);
    println!(
        "legacy_main_bytes={original} migrated_main_bytes={migrated} pending={}",
        recovered.pending_records
    );
    drop(spool);
    let reopened = fixture.open(60 * 1024 * 1024, 8);
    assert_eq!(
        reopened.status(namespace()).await.unwrap().pending_records,
        total
    );
}

#[tokio::test]
async fn near_full_many_small_legacy_rows_preserve_every_checkpoint_and_copy() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let database = fixture.directory.join("export.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=DELETE;
        CREATE TABLE binding (id INTEGER PRIMARY KEY CHECK(id=1), destination TEXT NOT NULL,
        revision INTEGER NOT NULL, kind TEXT NOT NULL, generation TEXT NOT NULL,
        critical_cursor TEXT, critical_seq INTEGER NOT NULL DEFAULT 0,
        telemetry_cursor TEXT, telemetry_seq INTEGER NOT NULL DEFAULT 0,
        critical_high INTEGER NOT NULL DEFAULT 0, telemetry_high INTEGER NOT NULL DEFAULT 0,
        incomplete INTEGER NOT NULL DEFAULT 0, legacy INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE pending (durability INTEGER NOT NULL, sequence INTEGER NOT NULL,
        payload BLOB NOT NULL, PRIMARY KEY(durability,sequence)) WITHOUT ROWID;
        CREATE TABLE gaps (durability INTEGER NOT NULL, first INTEGER NOT NULL, last INTEGER NOT NULL,
        reason INTEGER NOT NULL, CHECK(first > 0 AND last >= first), PRIMARY KEY(durability,first)) WITHOUT ROWID;
        INSERT INTO binding(id,destination,revision,kind,generation)
        VALUES(1,'analytics',1,'postgresql','source-test');
        PRAGMA user_version=1; BEGIN IMMEDIATE;").unwrap();
    let row = format!(
        "{{\"record_id\":\"small\",\"committed_at\":\"at\",\"record\":\"{}\"}}",
        "s".repeat(2800)
    );
    let mut count = 0_i64;
    let original = loop {
        count += 1;
        connection
            .execute(
                "INSERT INTO pending VALUES(0,?1,?2)",
                params![count, row.as_bytes()],
            )
            .unwrap();
        if count % 32 == 0 {
            let pages: i64 = connection
                .pragma_query_value(None, "page_count", |page| page.get(0))
                .unwrap();
            let bytes = u64::try_from(pages).expect("nonnegative SQLite page count") * 4096;
            if bytes >= 54 * 1024 * 1024 {
                break bytes;
            }
        }
        assert!(
            count <= 20_000,
            "small-row fixture did not reach near-full limit"
        );
    };
    assert!(original < 57 * 1024 * 1024);
    let count_u64 = u64::try_from(count).expect("positive fixture sequence");
    connection
        .execute(
            "UPDATE binding SET critical_cursor=?1,critical_seq=?2,critical_high=?2 WHERE id=1",
            params![
                checkpoint(Durability::Critical, count_u64).cursor.as_str(),
                count
            ],
        )
        .unwrap();
    connection.execute_batch("COMMIT").unwrap();
    drop(connection);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();

    let spool = fixture.open(60 * 1024 * 1024, 8);
    let status = spool.status(namespace()).await.unwrap();
    assert_eq!(status.pending_records, count_u64);
    assert_eq!(status.critical.unwrap().sequence, count_u64);
    assert!(status.gaps.is_empty());
    let migrated = fs::metadata(&database).unwrap().len();
    assert!(migrated <= 60 * 1024 * 1024);
    println!(
        "small_legacy_main_bytes={original} small_migrated_main_bytes={migrated} pending={count}"
    );
}
