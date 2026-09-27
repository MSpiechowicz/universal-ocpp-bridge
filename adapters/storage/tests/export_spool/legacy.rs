use rusqlite::{Connection, params};
use std::{fs, os::unix::fs::PermissionsExt};
use uob_application::{
    CommittedRecordField, Durability, ExportSpool, ExportSpoolErrorCode, PageLimit,
};

use super::support::{Fixture, checkpoint, namespace};

fn create_legacy(fixture: &Fixture, rows: &[(i64, i64, String)], main_bytes: u64) {
    let connection = Connection::open(fixture.directory.join("export.sqlite3")).unwrap();
    connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
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
        PRAGMA user_version=1;").unwrap();
    let row_count = i64::try_from(rows.len()).expect("fixture row count fits SQLite integer");
    connection.execute("INSERT INTO binding(id,destination,revision,kind,generation,critical_cursor,
        critical_seq,critical_high,incomplete) VALUES(1,'analytics',1,'postgresql','source-test',?1,?2,?2,1)",
        params![checkpoint(Durability::Critical, u64::try_from(row_count).unwrap()).cursor.as_str(), row_count]).unwrap();
    connection
        .execute("INSERT INTO gaps VALUES(1,1,2,0)", [])
        .unwrap();
    for (durability, sequence, payload) in rows {
        connection
            .execute(
                "INSERT INTO pending VALUES(?1,?2,?3)",
                params![durability, sequence, payload.as_bytes()],
            )
            .unwrap();
    }
    let used = u64::try_from(
        connection
            .pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))
            .unwrap(),
    )
    .expect("nonnegative SQLite page count")
        * 4096;
    assert!(used <= main_bytes);
    fs::set_permissions(
        fixture.directory.join("export.sqlite3"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

#[tokio::test]
async fn v1_upgrade_preserves_pending_fields_and_durable_facts() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let raw =
        r#"{"record_id":"id-é","committed_at":"2026-09-27T00:00:00Z","record":{"a":"\\u2603"}}"#;
    create_legacy(&fixture, &[(0, 1, raw.into())], 4 * 1024 * 1024);
    let spool = fixture.open(4 * 1024 * 1024, 8);
    let status = spool.status(namespace()).await.unwrap();
    assert_eq!(status.pending_records, 1);
    assert!(status.incomplete);
    assert_eq!(status.gaps[0].count(), 2);
    let item = spool
        .pending(namespace(), None, PageLimit::new(1).unwrap())
        .await
        .unwrap()
        .items
        .remove(0);
    let chunk = spool
        .pending_chunk(
            namespace(),
            item.clone(),
            CommittedRecordField::RecordId,
            0,
            64,
        )
        .await
        .unwrap();
    assert_eq!(chunk.bytes, "id-é".as_bytes());
    let data = spool
        .pending_chunk(namespace(), item, CommittedRecordField::Payload, 0, 64)
        .await
        .unwrap();
    assert_eq!(data.bytes, br#"{"a":"\\u2603"}"#);
    drop(spool);
    let version: i64 = Connection::open(fixture.directory.join("export.sqlite3"))
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

#[test]
fn malformed_oversize_legacy_row_rolls_back_to_recoverable_v1() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let oversized = format!(
        "{{\"record_id\":\"id\",\"committed_at\":\"at\",\"record\":\"{}\"}}",
        "x".repeat(1024 * 1024)
    );
    create_legacy(&fixture, &[(0, 1, oversized)], 4 * 1024 * 1024);
    let Err(error) = uob_storage_adapter::SqliteExportSpool::open_with_limits(
        &fixture.directory,
        &fixture.source,
        8,
        uob_storage_adapter::ExportSpoolLimits::new(4 * 1024 * 1024, 8).unwrap(),
        &fixture.budget,
    ) else {
        panic!("malformed legacy row was accepted");
    };
    assert_eq!(error.code(), ExportSpoolErrorCode::IntegrityFailure);
    let connection = Connection::open(fixture.directory.join("export.sqlite3")).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let count: i64 = connection
        .query_row("SELECT count(*) FROM pending", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
