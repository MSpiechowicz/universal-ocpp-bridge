#![cfg(target_os = "linux")]
#[path = "export_spool/delivery.rs"]
mod delivery;
#[path = "export_spool/device_isolation.rs"]
mod device_isolation;
#[path = "export_spool/legacy.rs"]
mod legacy;
#[path = "export_spool/near_full.rs"]
mod near_full;
#[path = "export_spool/support.rs"]
mod support;

use uob_application::{
    BudgetedRecordChunk, CommittedRecordField, Durability, ExportGap, ExportGapReason,
    ExportPendingPosition, ExportSpool, ExportSpoolErrorCode, PageLimit, WorkClass,
};

use support::{Fixture, append_field, begin, checkpoint, copy, namespace, progress, transfer};

#[tokio::test]
async fn multi_megabyte_record_and_successor_read_exact_fields_after_restart() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(12 * 1024 * 1024, 8);
    let id = "record-id".repeat(12_000);
    let date = b"\"2026-09-27T00:00:00Z\"";
    let payload = vec![b'p'; 3 * 1024 * 1024 + 29];
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        1,
        [id.as_bytes(), date, &payload],
    )
    .await;
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        2,
        [b"second", date, b"{}"],
    )
    .await;
    let expected = spool.status(namespace()).await.unwrap();
    assert_eq!(expected.critical.as_ref().unwrap().sequence, 2);
    assert_eq!(expected.pending_records, 2);
    drop(spool);
    let reopened = fixture.open(12 * 1024 * 1024, 8);
    assert_eq!(expected, reopened.status(namespace()).await.unwrap());
    let page = reopened
        .pending(namespace(), None, PageLimit::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.has_more);
    assert_eq!(
        page.resume,
        Some(ExportPendingPosition {
            durability: Durability::Critical,
            sequence: 1
        })
    );
    let item = page.items[0].clone();
    for (field, source) in [
        (CommittedRecordField::RecordId, id.as_bytes()),
        (CommittedRecordField::CommittedAt, date.as_slice()),
        (CommittedRecordField::Payload, payload.as_slice()),
    ] {
        let mut received = Vec::new();
        while received.len() < source.len() {
            let chunk = reopened
                .pending_chunk(
                    namespace(),
                    item.clone(),
                    field,
                    received.len() as u64,
                    64 * 1024,
                )
                .await
                .unwrap();
            received.extend_from_slice(&chunk.bytes);
        }
        assert_eq!(received, source);
    }
    let end = reopened
        .pending_chunk(
            namespace(),
            item.clone(),
            CommittedRecordField::Payload,
            payload.len() as u64,
            64 * 1024,
        )
        .await
        .unwrap();
    assert!(end.bytes.is_empty() && end.end_of_field);
    let next = reopened
        .pending(namespace(), page.resume, PageLimit::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(next.items[0].position.sequence, 2);
    assert!(!next.has_more);
    let bad = reopened
        .pending_chunk(
            namespace(),
            item,
            CommittedRecordField::RecordId,
            id.len() as u64 + 1,
            1,
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code(), ExportSpoolErrorCode::InvalidRequest);
}

#[tokio::test]
async fn impossible_critical_does_not_consume_chunks_or_advance_observed_high_water() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(2 * 1024 * 1024, 8);
    let observed = spool
        .observe(namespace(), Durability::Critical, 5, false)
        .await
        .unwrap();
    let attempt = spool
        .begin_record(begin(Durability::Critical, 1, [1, 1, 2 * 1024 * 1024]))
        .await
        .err()
        .unwrap();
    assert_eq!(attempt.code(), ExportSpoolErrorCode::Backpressure);
    assert_eq!(observed, spool.status(namespace()).await.unwrap());
    drop(spool);
    let reopened = fixture.open(2 * 1024 * 1024, 8);
    assert_eq!(observed, reopened.status(namespace()).await.unwrap());
    let telemetry = reopened
        .begin_record(begin(
            Durability::BestEffortTelemetry,
            1,
            [1, 1, 2 * 1024 * 1024],
        ))
        .await
        .unwrap();
    let uob_application::ExportSpoolRecordAdmission::TelemetryDropped(committed) = telemetry else {
        panic!("telemetry record unexpectedly admitted");
    };
    let status = reopened.status(namespace()).await.unwrap();
    assert_eq!(*committed, status);
    assert_eq!(status.critical_high_water, 5);
    assert!(status.critical.is_none());
    assert_eq!(status.telemetry.unwrap().sequence, 1);
    assert_eq!(status.gaps[0].reason, ExportGapReason::TelemetryDropped);
}

#[tokio::test]
async fn cancelled_partial_transfer_rolls_back_and_releases_worker() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(4 * 1024 * 1024, 8);
    spool
        .observe(namespace(), Durability::Critical, 1, false)
        .await
        .unwrap();
    let mut first = transfer(
        spool
            .begin_record(begin(Durability::Critical, 1, [2, 2, 1024]))
            .await
            .unwrap(),
    );
    append_field(
        &mut first,
        &fixture.budget,
        CommittedRecordField::RecordId,
        b"id",
    )
    .await;
    assert_eq!(
        spool.status(namespace()).await.unwrap_err().code(),
        ExportSpoolErrorCode::Busy
    );
    drop(first);
    let prior = spool.status(namespace()).await.unwrap();
    assert_eq!(prior.pending_records, 0);
    assert!(prior.critical.is_none());
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        1,
        [b"id", b"at", &vec![b'x'; 1024]],
    )
    .await;
    assert_eq!(spool.status(namespace()).await.unwrap().pending_records, 1);
    drop(spool);
    let reopened = fixture.open(4 * 1024 * 1024, 8);
    assert_eq!(
        reopened.status(namespace()).await.unwrap().pending_records,
        1
    );
}

#[tokio::test]
async fn lost_begin_and_finish_receivers_leave_only_complete_durable_states() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(4 * 1024 * 1024, 8);
    spool
        .observe(namespace(), Durability::Critical, 1, false)
        .await
        .unwrap();
    drop(spool.begin_record(begin(Durability::Critical, 1, [2, 2, 2])));
    let before = spool.status(namespace()).await.unwrap();
    assert_eq!(before.pending_records, 0);
    assert!(before.critical.is_none());

    let mut transfer = transfer(
        spool
            .begin_record(begin(Durability::Critical, 1, [2, 2, 2]))
            .await
            .unwrap(),
    );
    append_field(
        &mut transfer,
        &fixture.budget,
        CommittedRecordField::RecordId,
        b"id",
    )
    .await;
    append_field(
        &mut transfer,
        &fixture.budget,
        CommittedRecordField::CommittedAt,
        b"at",
    )
    .await;
    append_field(
        &mut transfer,
        &fixture.budget,
        CommittedRecordField::Payload,
        b"{}",
    )
    .await;
    drop(transfer.finish());
    let after = spool.status(namespace()).await.unwrap();
    assert!(after.pending_records <= 1);
    assert_eq!(
        after.critical.as_ref().map(|point| point.sequence),
        (after.pending_records == 1).then_some(1)
    );
    drop(spool);
    let reopened = fixture.open(4 * 1024 * 1024, 8);
    assert_eq!(reopened.status(namespace()).await.unwrap(), after);
}

#[tokio::test]
async fn mutated_or_reordered_chunks_and_incomplete_finish_rollback() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(4 * 1024 * 1024, 8);
    spool
        .observe(namespace(), Durability::Critical, 1, false)
        .await
        .unwrap();
    let mut transfer = transfer(
        spool
            .begin_record(begin(Durability::Critical, 1, [2, 2, 2]))
            .await
            .unwrap(),
    );
    let guard = fixture
        .budget
        .try_reserve(WorkClass::ExporterBatch, 2)
        .unwrap();
    let out_of_order =
        BudgetedRecordChunk::new(CommittedRecordField::Payload, 0, b"{}".to_vec(), 2, guard)
            .unwrap();
    assert_eq!(
        transfer.append(out_of_order).await.unwrap_err().code(),
        ExportSpoolErrorCode::InvalidRequest
    );
    drop(transfer);
    assert_eq!(spool.status(namespace()).await.unwrap().pending_records, 0);

    let mut transfer = support::transfer(
        spool
            .begin_record(begin(Durability::Critical, 1, [2, 2, 2]))
            .await
            .unwrap(),
    );
    let guard = fixture
        .budget
        .try_reserve(WorkClass::ExporterBatch, 1)
        .unwrap();
    let mut mutated =
        BudgetedRecordChunk::new(CommittedRecordField::RecordId, 0, b"i".to_vec(), 2, guard)
            .unwrap();
    mutated.bytes.push(b'd');
    mutated.next_offset = 2;
    mutated.end_of_field = true;
    assert_eq!(
        transfer.append(mutated).await.unwrap_err().code(),
        ExportSpoolErrorCode::InvalidRequest
    );
    drop(transfer);
    assert_eq!(spool.status(namespace()).await.unwrap().pending_records, 0);

    let mut transfer = support::transfer(
        spool
            .begin_record(begin(Durability::Critical, 1, [2, 2, 2]))
            .await
            .unwrap(),
    );
    append_field(
        &mut transfer,
        &fixture.budget,
        CommittedRecordField::RecordId,
        b"id",
    )
    .await;
    assert_eq!(
        transfer.finish().await.unwrap_err().code(),
        ExportSpoolErrorCode::InvalidRequest
    );
    let status = spool.status(namespace()).await.unwrap();
    assert_eq!(status.pending_records, 0);
    assert!(status.critical.is_none());
}

#[tokio::test]
async fn gap_coverage_namespace_and_checkpoint_validation_are_atomic() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(2 * 1024 * 1024, 2);
    let mut leading = progress(Durability::Critical, 2);
    leading.expected = None;
    leading.gaps.push(ExportGap {
        durability: Durability::Critical,
        first: 1,
        last: 2,
        reason: ExportGapReason::SourceExpired,
    });
    let status = spool.commit_gaps(leading).await.unwrap();
    assert_eq!(status.critical.unwrap().sequence, 2);
    assert!(status.incomplete);
    let mut next = progress(Durability::Critical, 3);
    next.expected = Some(checkpoint(Durability::Critical, 2));
    let wrong = spool.commit_gaps(next).await.unwrap_err();
    assert_eq!(wrong.code(), ExportSpoolErrorCode::InvalidRequest);
    let stale = spool
        .begin_record(begin(Durability::Critical, 1, [1, 1, 1]))
        .await
        .err()
        .unwrap();
    assert_eq!(stale.code(), ExportSpoolErrorCode::CheckpointConflict);
    let mut other = namespace();
    other.destination.configuration_revision = 2;
    assert_eq!(
        spool.status(other).await.unwrap_err().code(),
        ExportSpoolErrorCode::NamespaceConflict
    );
}

#[tokio::test]
async fn saturated_source_gap_summary_never_advances_checkpoint() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let spool = fixture.open(2 * 1024 * 1024, 2);
    let gap = |sequence| ExportGap {
        durability: Durability::Critical,
        first: sequence,
        last: sequence,
        reason: ExportGapReason::SourceExpired,
    };
    let mut first = progress(Durability::Critical, 1);
    first.gaps = vec![gap(1)];
    spool.commit_gaps(first).await.unwrap();
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        2,
        [b"id", b"at", b"{}"],
    )
    .await;
    let mut third = progress(Durability::Critical, 3);
    third.gaps = vec![gap(3)];
    spool.commit_gaps(third).await.unwrap();
    copy(
        &spool,
        &fixture.budget,
        Durability::Critical,
        4,
        [b"id", b"at", b"{}"],
    )
    .await;
    let prior = spool.status(namespace()).await.unwrap();
    let mut fifth = progress(Durability::Critical, 5);
    fifth.gaps = vec![gap(5)];
    let error = spool.commit_gaps(fifth).await.unwrap_err();
    assert_eq!(error.code(), ExportSpoolErrorCode::SummaryFull);
    assert_eq!(spool.status(namespace()).await.unwrap(), prior);
}
