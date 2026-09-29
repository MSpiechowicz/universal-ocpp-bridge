use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
use uob_application::{Durability, ExportSpool, ExportSpoolErrorCode, PageLimit};
use uob_contracts::{ExportBatchId, ExportReport};

use super::support::{Fixture, copy, namespace};

fn report(batch: &ExportBatchId, identities: &[&str], outcome: &Value) -> ExportReport {
    serde_json::from_value(json!({
        "batch_id": batch.as_str(),
        "destination": namespace().destination,
        "record_ids": identities.iter().map(|id| json!({"record_id": id})).collect::<Vec<_>>(),
        "outcome": outcome,
    }))
    .unwrap()
}

fn committed(batch: &ExportBatchId, identities: &[&str]) -> ExportReport {
    report(batch, identities, &json!({"outcome": "committed"}))
}

async fn insert(
    fixture: &Fixture,
    spool: &uob_storage_adapter::SqliteExportSpool,
    durability: Durability,
    sequence: u64,
    id: &str,
    payload: &[u8],
) {
    copy(
        spool,
        &fixture.budget,
        durability,
        sequence,
        [id.as_bytes(), b"2026-09-29T00:00:00Z", payload],
    )
    .await;
}

async fn claim_initial_prefix(fixture: &Fixture) -> ExportBatchId {
    let spool = fixture.open(4 * 1024 * 1024, 8);
    insert(fixture, &spool, Durability::Critical, 1, "first", b"{}").await;
    insert(fixture, &spool, Durability::Critical, 2, "second", b"{}").await;
    insert(
        fixture,
        &spool,
        Durability::BestEffortTelemetry,
        1,
        "telemetry",
        b"{}",
    )
    .await;

    let claim = spool
        .claim_delivery(namespace(), PageLimit::new(2).unwrap(), 256 * 1024)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        claim
            .items
            .iter()
            .map(|item| item.position.sequence)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let id = claim.batch_id.clone();
    drop(claim);
    drop(spool);
    id
}

async fn assert_invalid_confirmations(
    spool: &uob_storage_adapter::SqliteExportSpool,
    id: &ExportBatchId,
) {
    let stale = ExportBatchId::new("stale").unwrap();
    assert_eq!(
        spool
            .settle_delivery(namespace(), committed(&stale, &["first", "second"]))
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::CheckpointConflict
    );
    assert_eq!(
        spool
            .settle_delivery(namespace(), committed(id, &["second", "first"]))
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::CheckpointConflict
    );
    let mut wrong = namespace();
    wrong.destination.configuration_revision += 1;
    assert_eq!(
        spool
            .settle_delivery(wrong, committed(id, &["first", "second"]))
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::NamespaceConflict
    );
    let different_destination: ExportReport = serde_json::from_value(json!({
        "batch_id":id.as_str(),
        "destination":{"destination_id":"another","configuration_revision":1},
        "record_ids":[{"record_id":"first"},{"record_id":"second"}],
        "outcome":{"outcome":"committed"}
    }))
    .unwrap();
    assert_eq!(
        spool
            .settle_delivery(namespace(), different_destination)
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::CheckpointConflict
    );
}

#[tokio::test]
async fn claimed_prefix_replays_after_restart_and_exact_commit_only_advances_remote_progress() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = claim_initial_prefix(&fixture).await;

    let reopened = fixture.open(4 * 1024 * 1024, 8);
    let replay = reopened
        .claim_delivery(namespace(), PageLimit::new(2).unwrap(), 256 * 1024)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay.batch_id, id);
    assert_eq!(replay.items.len(), 2);
    let retry = report(
        &id,
        &["first", "second"],
        &json!({"outcome":"uncertain", "stage":"commit_outcome_unknown", "error":"timeout"}),
    );
    let before = reopened.status(namespace()).await.unwrap();
    assert_eq!(
        reopened.settle_delivery(namespace(), retry).await.unwrap(),
        before
    );
    drop(reopened);

    let reopened = fixture.open(4 * 1024 * 1024, 8);
    assert_eq!(
        reopened
            .claim_delivery(namespace(), PageLimit::new(2).unwrap(), 256 * 1024)
            .await
            .unwrap()
            .unwrap()
            .batch_id,
        id
    );
    assert_invalid_confirmations(&reopened, &id).await;
    assert_eq!(reopened.status(namespace()).await.unwrap(), before);

    let confirmed = reopened
        .settle_delivery(namespace(), committed(&id, &["first", "second"]))
        .await
        .unwrap();
    assert_eq!(confirmed.pending_records, 1);
    assert_eq!(confirmed.confirmed_records, 2);
    assert_eq!(confirmed.last_confirmed_batch, Some(id.clone()));
    assert_eq!(confirmed.critical.unwrap().sequence, 2); // Source cursor is not remote progress.
    assert_eq!(
        reopened
            .settle_delivery(namespace(), committed(&id, &["first", "second"]))
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::CheckpointConflict
    );
    drop(reopened);
    let reopened = fixture.open(4 * 1024 * 1024, 8);
    assert_eq!(
        reopened
            .status(namespace())
            .await
            .unwrap()
            .confirmed_records,
        2
    );
    let suffix = reopened
        .claim_delivery(namespace(), PageLimit::new(2).unwrap(), 256 * 1024)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(suffix.batch_id, id);
    assert_eq!(suffix.items.len(), 1);
    assert_eq!(
        suffix.items[0].position.durability,
        Durability::BestEffortTelemetry
    );
}

#[tokio::test]
async fn pressure_cannot_evict_claimed_telemetry_but_evicts_unclaimed_telemetry() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(1024 * 1024, 8);
    insert(
        &fixture,
        &spool,
        Durability::BestEffortTelemetry,
        1,
        "protected",
        b"{}",
    )
    .await;
    let claim = spool
        .claim_delivery(namespace(), PageLimit::new(1).unwrap(), 256 * 1024)
        .await
        .unwrap()
        .unwrap();
    let protected = claim.items[0].clone();
    let bulk = vec![b'x'; 100 * 1024];
    for sequence in 2..=6 {
        insert(
            &fixture,
            &spool,
            Durability::BestEffortTelemetry,
            sequence,
            &format!("evictable-{sequence}"),
            &bulk,
        )
        .await;
    }
    let critical = vec![b'y'; 350 * 1024];
    insert(
        &fixture,
        &spool,
        Durability::Critical,
        1,
        "critical",
        &critical,
    )
    .await;
    let status = spool.status(namespace()).await.unwrap();
    assert!(
        status
            .gaps
            .iter()
            .any(|gap| gap.durability == Durability::BestEffortTelemetry
                && gap.first >= 2
                && gap.reason == uob_application::ExportGapReason::TelemetryEvicted)
    );
    let pinned = spool
        .pending_chunk(
            namespace(),
            protected,
            uob_application::CommittedRecordField::RecordId,
            0,
            32,
        )
        .await
        .unwrap();
    assert_eq!(pinned.bytes, b"protected");
    assert_eq!(
        spool
            .claim_delivery(namespace(), PageLimit::new(1).unwrap(), 256 * 1024)
            .await
            .unwrap()
            .unwrap()
            .batch_id,
        claim.batch_id
    );
}

#[tokio::test]
async fn oversized_critical_delivery_remains_pending_without_a_false_confirmation() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(4 * 1024 * 1024, 8);
    let large = vec![b'z'; 64 * 1024];
    insert(
        &fixture,
        &spool,
        Durability::Critical,
        1,
        "oversize",
        &large,
    )
    .await;
    assert_eq!(
        spool
            .claim_delivery(namespace(), PageLimit::new(1).unwrap(), 256 * 1024)
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::DeliveryLimitExceeded
    );
    let status = spool.status(namespace()).await.unwrap();
    assert_eq!(status.pending_records, 1);
    assert_eq!(status.critical.unwrap().sequence, 1);
    assert_eq!(status.confirmed_records, 0);
}

#[tokio::test]
async fn v2_upgrade_retains_pending_and_accepts_exact_remote_confirmation() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let path = fixture.directory.join("export.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=DELETE;
        CREATE TABLE binding (id INTEGER PRIMARY KEY CHECK(id=1), destination TEXT NOT NULL,
            revision INTEGER NOT NULL, kind TEXT NOT NULL, generation TEXT NOT NULL,
            critical_cursor TEXT, critical_seq INTEGER NOT NULL DEFAULT 0,
            telemetry_cursor TEXT, telemetry_seq INTEGER NOT NULL DEFAULT 0,
            critical_high INTEGER NOT NULL DEFAULT 0, telemetry_high INTEGER NOT NULL DEFAULT 0,
            incomplete INTEGER NOT NULL DEFAULT 0, legacy INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE pending (row_id INTEGER PRIMARY KEY, durability INTEGER NOT NULL,
            sequence INTEGER NOT NULL, record_id BLOB NOT NULL, committed_at BLOB NOT NULL,
            payload BLOB NOT NULL, UNIQUE(durability,sequence));
        CREATE TABLE gaps (durability INTEGER NOT NULL, first INTEGER NOT NULL, last INTEGER NOT NULL,
            reason INTEGER NOT NULL, PRIMARY KEY(durability,first)) WITHOUT ROWID;
        PRAGMA user_version=2;").unwrap();
    db.execute(
        "INSERT INTO binding(id,destination,revision,kind,generation,critical_cursor,
        critical_seq,critical_high) VALUES(1,'analytics',1,'postgresql','source-test',?1,1,1)",
        [super::support::checkpoint(Durability::Critical, 1)
            .cursor
            .as_str()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO pending(durability,sequence,record_id,committed_at,payload)
        VALUES(0,1,?1,?2,?3)",
        params![b"legacy-id", b"2026-09-29T00:00:00Z", b"{}"],
    )
    .unwrap();
    drop(db);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let spool = fixture.open(4 * 1024 * 1024, 8);
    let claim = spool
        .claim_delivery(namespace(), PageLimit::new(1).unwrap(), 256 * 1024)
        .await
        .unwrap()
        .unwrap();
    let status = spool
        .settle_delivery(namespace(), committed(&claim.batch_id, &["legacy-id"]))
        .await
        .unwrap();
    assert_eq!(status.pending_records, 0);
    assert_eq!(status.confirmed_records, 1);
    assert_eq!(status.critical.unwrap().sequence, 1);
    drop(spool);
    let version: i64 = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
}
