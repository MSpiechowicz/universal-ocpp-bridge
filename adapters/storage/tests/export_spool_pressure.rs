#![cfg(target_os = "linux")]
#[path = "export_spool/support.rs"]
mod support;

use std::fs;

use uob_application::{
    CommittedRecordField, Durability, ExportGapReason, ExportSpool, ExportSpoolErrorCode, PageLimit,
};

use support::{Fixture, begin, copy, namespace, transfer};

#[tokio::test]
async fn temporary_pressure_evicts_telemetry_with_exact_gaps_and_never_critical() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(1024 * 1024, 32);
    let telemetry_payload = vec![b't'; 90 * 1024];
    for sequence in 1..=5 {
        copy(
            &spool,
            &fixture.budget,
            Durability::BestEffortTelemetry,
            sequence,
            [b"telemetry", b"at", &telemetry_payload],
        )
        .await;
    }
    let previous = spool
        .pending(namespace(), None, PageLimit::new(1).unwrap())
        .await
        .unwrap()
        .items
        .remove(0);
    let critical_payload = vec![b'c'; 400 * 1024];
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        1,
        [b"critical", b"at", &critical_payload],
    )
    .await;
    let status = spool.status(namespace()).await.unwrap();
    assert_eq!(status.critical.unwrap().sequence, 1);
    assert!(
        status
            .gaps
            .iter()
            .any(|gap| gap.reason == ExportGapReason::TelemetryEvicted)
    );
    let lost: u64 = status
        .gaps
        .iter()
        .filter(|gap| gap.reason == ExportGapReason::TelemetryEvicted)
        .map(uob_application::ExportGap::count)
        .sum();
    assert_eq!(status.pending_records + lost, 6);
    assert_eq!(
        spool
            .pending_chunk(namespace(), previous, CommittedRecordField::Payload, 0, 1)
            .await
            .unwrap_err()
            .code(),
        ExportSpoolErrorCode::PendingExpired
    );
    let file_size = fs::metadata(fixture.directory.join("export.sqlite3"))
        .unwrap()
        .len();
    assert!(file_size <= 1024 * 1024);
}

#[tokio::test]
async fn many_telemetry_evictions_preserve_the_exact_surviving_suffix() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(1024 * 1024, 32);
    let telemetry_payload = vec![b't'; 8 * 1024];
    for sequence in 1..=64 {
        copy(
            &spool,
            &fixture.budget,
            Durability::BestEffortTelemetry,
            sequence,
            [b"telemetry", b"at", &telemetry_payload],
        )
        .await;
    }
    assert_eq!(spool.status(namespace()).await.unwrap().pending_records, 64);

    let critical_payload = vec![b'c'; 512 * 1024];
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        1,
        [b"critical", b"at", &critical_payload],
    )
    .await;

    let status = spool.status(namespace()).await.unwrap();
    let evicted: Vec<_> = status
        .gaps
        .iter()
        .filter(|gap| gap.reason == ExportGapReason::TelemetryEvicted)
        .collect();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].first, 1);
    let lost = evicted[0].last;
    assert!(lost >= 20, "expected a many-row eviction, got {lost}");
    assert!(lost < 64, "the telemetry suffix must survive");
    assert_eq!(status.pending_records + lost, 65);
    assert_eq!(status.critical.unwrap().sequence, 1);

    let pending = spool
        .pending(namespace(), None, PageLimit::new(64).unwrap())
        .await
        .unwrap();
    let telemetry: Vec<_> = pending
        .items
        .iter()
        .filter(|item| item.position.durability == Durability::BestEffortTelemetry)
        .map(|item| item.position.sequence)
        .collect();
    assert_eq!(telemetry, ((lost + 1)..=64).collect::<Vec<_>>());
}

#[tokio::test]
async fn provisional_evictions_rollback_when_transfer_is_aborted() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(1024 * 1024, 32);
    let payload = vec![b'z'; 90 * 1024];
    for sequence in 1..=5 {
        copy(
            &spool,
            &fixture.budget,
            Durability::BestEffortTelemetry,
            sequence,
            [b"telemetry", b"at", &payload],
        )
        .await;
    }
    let before = spool.status(namespace()).await.unwrap();
    let transfer = transfer(
        spool
            .begin_record(begin(Durability::Critical, 1, [8, 2, 400 * 1024]))
            .await
            .unwrap(),
    );
    transfer.abort().await.unwrap();
    assert_eq!(spool.status(namespace()).await.unwrap(), before);
    assert!(before.gaps.is_empty());
}

#[tokio::test]
async fn individually_impossible_critical_never_evicts_telemetry() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(1024 * 1024, 8);
    let payload = vec![b't'; 64 * 1024];
    copy(
        &spool,
        &fixture.budget,
        Durability::BestEffortTelemetry,
        1,
        [b"telemetry", b"at", &payload],
    )
    .await;
    let before = spool.status(namespace()).await.unwrap();
    let error = spool
        .begin_record(begin(Durability::Critical, 1, [10, 2, 900 * 1024]))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code(), ExportSpoolErrorCode::Backpressure);
    assert_eq!(spool.status(namespace()).await.unwrap(), before);
}
