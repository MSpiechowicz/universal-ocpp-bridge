use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::time::advance;
use uob_application::{
    AtomicStoreWrite, BudgetedRecordChunk, CommittedRecordField, CommittedRecordQuery, Durability,
    ExportDeliveryClaim, ExportPendingDescriptor, ExportPendingPage, ExportPendingPosition,
    ExportSourceCheckpoint, ExportSpool, ExportSpoolError, ExportSpoolErrorCode, ExportSpoolFuture,
    ExportSpoolGapCommit, ExportSpoolNamespace, ExportSpoolRecordAdmission, ExportSpoolRecordBegin,
    ExportSpoolStatus, OperationalStore, PageLimit, StorageWritePurpose,
};
use uob_contracts::ExportReport;
use uob_storage_adapter::SqliteExportSpool;

use super::support::{
    Behavior, Control, Fixture, populated_spool, record, resources, start_with_spool, until,
};

// The backing store is the real durable spool; these gates make Busy and
// memory Backpressure phases deterministic without OS-thread timing races.
struct ContendedSpool {
    inner: Arc<SqliteExportSpool>,
    status_busy: AtomicUsize,
    claim_busy: AtomicUsize,
    claim_pressure: AtomicUsize,
    chunk_busy: AtomicUsize,
    chunk_pressure: AtomicUsize,
    settle_busy: AtomicUsize,
}

impl ContendedSpool {
    fn new(
        inner: Arc<SqliteExportSpool>,
        status: usize,
        claim: usize,
        chunk: usize,
        settle: usize,
    ) -> Self {
        Self {
            inner,
            status_busy: AtomicUsize::new(status),
            claim_busy: AtomicUsize::new(claim),
            claim_pressure: AtomicUsize::new(0),
            chunk_busy: AtomicUsize::new(chunk),
            chunk_pressure: AtomicUsize::new(0),
            settle_busy: AtomicUsize::new(settle),
        }
    }

    fn busy(counter: &AtomicUsize) -> bool {
        counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

fn spool_error<T: Send + 'static>(code: ExportSpoolErrorCode) -> ExportSpoolFuture<'static, T> {
    Box::pin(async move { Err(ExportSpoolError::new(code, "transient spool contention")) })
}

fn busy<T: Send + 'static>() -> ExportSpoolFuture<'static, T> {
    spool_error(ExportSpoolErrorCode::Busy)
}

impl ExportSpool for ContendedSpool {
    fn status(&self, namespace: ExportSpoolNamespace) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        if Self::busy(&self.status_busy) {
            busy()
        } else {
            self.inner.status(namespace)
        }
    }

    fn observe(
        &self,
        namespace: ExportSpoolNamespace,
        durability: Durability,
        high_water: u64,
        legacy_baseline_incomplete: bool,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.inner.observe(
            namespace,
            durability,
            high_water,
            legacy_baseline_incomplete,
        )
    }

    fn commit_gaps(
        &self,
        request: ExportSpoolGapCommit,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.inner.commit_gaps(request)
    }

    fn begin_record(
        &self,
        request: ExportSpoolRecordBegin,
    ) -> ExportSpoolFuture<'_, ExportSpoolRecordAdmission> {
        self.inner.begin_record(request)
    }

    fn pending(
        &self,
        namespace: ExportSpoolNamespace,
        after: Option<ExportPendingPosition>,
        limit: PageLimit,
    ) -> ExportSpoolFuture<'_, ExportPendingPage> {
        self.inner.pending(namespace, after, limit)
    }

    fn pending_chunk(
        &self,
        namespace: ExportSpoolNamespace,
        descriptor: ExportPendingDescriptor,
        field: CommittedRecordField,
        offset: u64,
        max_bytes: usize,
    ) -> ExportSpoolFuture<'_, BudgetedRecordChunk> {
        if Self::busy(&self.chunk_pressure) {
            spool_error(ExportSpoolErrorCode::Backpressure)
        } else if Self::busy(&self.chunk_busy) {
            busy()
        } else {
            self.inner
                .pending_chunk(namespace, descriptor, field, offset, max_bytes)
        }
    }

    fn claim_delivery(
        &self,
        namespace: ExportSpoolNamespace,
        limit: PageLimit,
        max_bytes: usize,
    ) -> ExportSpoolFuture<'_, Option<ExportDeliveryClaim>> {
        if Self::busy(&self.claim_pressure) {
            spool_error(ExportSpoolErrorCode::Backpressure)
        } else if Self::busy(&self.claim_busy) {
            busy()
        } else {
            self.inner.claim_delivery(namespace, limit, max_bytes)
        }
    }

    fn settle_delivery(
        &self,
        namespace: ExportSpoolNamespace,
        report: ExportReport,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        if Self::busy(&self.settle_busy) {
            busy()
        } else {
            self.inner.settle_delivery(namespace, report)
        }
    }
}

#[tokio::test(start_paused = true)]
async fn transient_busy_at_each_spool_phase_preserves_one_remote_commit() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let gated = Arc::new(ContendedSpool::new(spool.clone(), 1, 1, 1, 2));
    let control = Arc::new(Control::default());
    let handle = start_with_spool(
        &control,
        Behavior::Commit,
        &namespace,
        gated.clone(),
        &resources,
    );

    for retries in 1..=5 {
        until(|| handle.health().retry_count >= retries).await;
        assert_ne!(
            handle.health().provider.state,
            uob_application::DatabaseHealthState::Ready
        );
        tokio::task::yield_now().await;
        advance(Duration::from_millis(250)).await;
    }
    until(|| handle.health().confirmed_records == 2).await;
    assert_eq!(control.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(gated.settle_busy.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    assert_eq!(
        spool
            .status(namespace.clone())
            .await
            .unwrap()
            .pending_records,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn transient_claim_and_load_backpressure_recover_without_replaying_provider() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let gated = Arc::new(ContendedSpool::new(spool.clone(), 0, 0, 0, 0));
    gated.claim_pressure.store(1, Ordering::SeqCst);
    gated.chunk_pressure.store(1, Ordering::SeqCst);
    let control = Arc::new(Control::default());
    let handle = start_with_spool(
        &control,
        Behavior::Commit,
        &namespace,
        gated.clone(),
        &resources,
    );

    for retries in 1..=2 {
        until(|| handle.health().retry_count == retries).await;
        assert_eq!(
            handle.health().provider.reason.as_deref(),
            Some("spool.memory_pressure")
        );
        assert_eq!(control.attempts.load(Ordering::SeqCst), 0);
        advance(Duration::from_millis(250)).await;
    }
    until(|| handle.health().confirmed_records == 2).await;
    assert_eq!(control.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(gated.claim_pressure.load(Ordering::SeqCst), 0);
    assert_eq!(gated.chunk_pressure.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 0);
}

#[tokio::test(start_paused = true)]
async fn shutdown_cancels_busy_settlement_without_replaying_provider() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let gated = Arc::new(ContendedSpool::new(spool.clone(), 0, 0, 0, usize::MAX));
    let control = Arc::new(Control::default());
    let handle = start_with_spool(&control, Behavior::Commit, &namespace, gated, &resources);
    until(|| handle.health().retry_count > 0).await;
    assert_eq!(control.attempts.load(Ordering::SeqCst), 1);
    handle.shutdown().await.unwrap();
    assert_eq!(spool.status(namespace).await.unwrap().pending_records, 2);
}

#[tokio::test(start_paused = true)]
async fn real_provisional_ingestion_transfer_does_not_terminate_scheduler() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let resources = resources();
    let (spool, namespace) = populated_spool(&fixture, &resources).await;
    let source = fixture.source();
    source
        .write_atomic(AtomicStoreWrite {
            charging_profile_201: None,
            reservation_16: None,
            reservation_observations_16: Vec::new(),
            reservation_201: None,
            reservation_observations_201: Vec::new(),
            firmware_16: None,
            firmware_observations_16: Vec::new(),
            firmware_201: None,
            firmware_observations_201: Vec::new(),
            purpose: StorageWritePurpose::Routine,
            station_snapshot: None,
            authorization_changes: vec![],
            command: None,
            command_result: None,
            journal_events: vec![],
            required_deliveries: vec![],
            committed_records: vec![record("concurrent-transfer")],
        })
        .await
        .unwrap();

    let previous = spool.status(namespace.clone()).await.unwrap();
    let page = source
        .read_committed_records(
            CommittedRecordQuery {
                after: previous.critical.as_ref().map(|point| point.cursor.clone()),
                limit: PageLimit::new(1).unwrap(),
                durability: Durability::Critical,
            },
            &resources,
        )
        .await
        .unwrap();
    let descriptor = page.items.into_iter().next().unwrap();
    let transfer = spool
        .begin_record(ExportSpoolRecordBegin {
            progress: ExportSpoolGapCommit {
                namespace: namespace.clone(),
                durability: Durability::Critical,
                expected: previous.critical,
                next: ExportSourceCheckpoint {
                    cursor: descriptor.cursor.clone(),
                    sequence: descriptor.sequence,
                },
                high_water: page.high_water,
                gaps: vec![],
                legacy_baseline_incomplete: page.legacy_baseline_incomplete,
            },
            descriptor,
        })
        .await
        .unwrap();
    let ExportSpoolRecordAdmission::Transfer(transfer) = transfer else {
        panic!("critical record requires a transfer");
    };

    let control = Arc::new(Control::default());
    let handle = start_with_spool(
        &control,
        Behavior::Commit,
        &namespace,
        spool.clone(),
        &resources,
    );
    until(|| handle.health().retry_count == 1).await;
    assert_ne!(
        handle.health().provider.state,
        uob_application::DatabaseHealthState::Ready
    );
    assert_eq!(control.attempts.load(Ordering::SeqCst), 0);
    drop(transfer);
    tokio::task::yield_now().await;
    advance(Duration::from_millis(250)).await;
    until(|| handle.health().confirmed_records == 2).await;
    assert_eq!(control.attempts.load(Ordering::SeqCst), 1);
    handle.shutdown().await.unwrap();
}
