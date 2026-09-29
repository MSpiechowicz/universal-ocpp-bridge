use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uob_application::{
    BudgetedRecordChunk, ExportDeliveryClaim, ExportPendingPage, ExportPendingPosition,
    ExportSpoolError, ExportSpoolFuture, ExportSpoolGapCommit, ExportSpoolRecordAdmission,
    ExportSpoolRecordBegin, ExportSpoolStatus, ExportSpoolTransfer,
};
use uob_contracts::{
    DataPointValue, ExportReport, Freshness, PointId, Quality, QualityLevel, TypedValue,
};

// Inject retention only after the real spool has accepted and written the first field.
// The next real source chunk must observe expiration and force a full spool rollback.
struct ExpiringSpool {
    inner: SqliteExportSpool,
    source: Arc<Source>,
    expired: Arc<AtomicBool>,
    now: UtcTimestamp,
}

struct ExpiringTransfer {
    inner: Box<dyn ExportSpoolTransfer>,
    source: Arc<Source>,
    expired: Arc<AtomicBool>,
    now: UtcTimestamp,
}

impl ExportSpoolTransfer for ExpiringTransfer {
    fn append(&mut self, chunk: BudgetedRecordChunk) -> ExportSpoolFuture<'_, ()> {
        Box::pin(async move {
            self.inner.append(chunk).await?;
            if !self.expired.swap(true, Ordering::AcqRel) {
                self.source
                    .maintain_storage_retention(self.now)
                    .await
                    .map_err(|_| {
                        ExportSpoolError::new(
                            ExportSpoolErrorCode::Unavailable,
                            "source retention injection failed",
                        )
                    })?;
            }
            Ok(())
        })
    }

    fn finish(self: Box<Self>) -> ExportSpoolFuture<'static, ExportSpoolStatus> {
        self.inner.finish()
    }

    fn abort(self: Box<Self>) -> ExportSpoolFuture<'static, ()> {
        self.inner.abort()
    }
}

impl ExportSpool for ExpiringSpool {
    fn status(&self, namespace: ExportSpoolNamespace) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.inner.status(namespace)
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
        Box::pin(async move {
            match self.inner.begin_record(request).await? {
                ExportSpoolRecordAdmission::Transfer(inner) => Ok(
                    ExportSpoolRecordAdmission::Transfer(Box::new(ExpiringTransfer {
                        inner,
                        source: Arc::clone(&self.source),
                        expired: Arc::clone(&self.expired),
                        now: self.now,
                    })),
                ),
                ExportSpoolRecordAdmission::TelemetryDropped(status) => {
                    Ok(ExportSpoolRecordAdmission::TelemetryDropped(status))
                }
            }
        })
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
        self.inner
            .pending_chunk(namespace, descriptor, field, offset, max_bytes)
    }

    fn claim_delivery(
        &self,
        namespace: ExportSpoolNamespace,
        limit: PageLimit,
        max_bytes: usize,
    ) -> ExportSpoolFuture<'_, Option<ExportDeliveryClaim>> {
        self.inner.claim_delivery(namespace, limit, max_bytes)
    }

    fn settle_delivery(
        &self,
        namespace: ExportSpoolNamespace,
        report: ExportReport,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.inner.settle_delivery(namespace, report)
    }
}

fn text_record(id: &str, text: String) -> CommittedRecord<ExportRecord> {
    let mut row = record(id, Durability::Critical);
    row.record = ExportRecord::new(
        row.record.metadata().clone(),
        ExportPayload::Measurement(DataPointValue {
            point_id: PointId::new("meter-note").unwrap(),
            value: Some(TypedValue::Text(text)),
            source_time: None,
            observed_at: instant(),
            quality: Quality {
                level: QualityLevel::Good,
                reason: None,
            },
            freshness: Freshness::Unknown,
            measurement: None,
        }),
    );
    row
}

#[tokio::test]
async fn multi_megabyte_copy_preserves_every_field_and_successor_after_restart() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, spool) = fixture.open_with_size(8 * 1024 * 1024);
    let large = text_record("large", "🌍\\\"".repeat(800_000));
    let successor = record("after-large", Durability::Critical);
    let originals = [large.clone(), successor.clone()];
    commit(&source, vec![large, successor]).await;

    let copied =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert_eq!(copied.processed, 2);
    assert_eq!(copied.status.critical.as_ref().unwrap().sequence, 2);
    assert_eq!(copied.status.pending_records, 2);
    assert!(!copied.status.incomplete);
    let namespace = ExportSpoolNamespace {
        destination: destination(),
        provider_kind: "postgresql".into(),
        source_generation: copied.status.source_generation.clone(),
    };

    drop(spool);
    let spool = fixture.open_with_size(8 * 1024 * 1024).1;
    let first = spool
        .pending(namespace.clone(), None, PageLimit::new(1).unwrap())
        .await
        .unwrap();
    assert!(first.has_more);
    assert_eq!(first.items.len(), 1);
    let second = spool
        .pending(namespace.clone(), first.resume, PageLimit::new(1).unwrap())
        .await
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(second.items.len(), 1);
    for (row, original) in first.items.iter().chain(&second.items).zip(&originals) {
        assert_eq!(row.position.durability, Durability::Critical);
        assert_eq!(
            pending_field(&spool, &namespace, row, CommittedRecordField::RecordId).await,
            original.record_id.as_str().as_bytes()
        );
        assert_eq!(
            pending_field(&spool, &namespace, row, CommittedRecordField::CommittedAt).await,
            serde_json::to_vec(&original.committed_at).unwrap()
        );
        assert_eq!(
            pending_field(&spool, &namespace, row, CommittedRecordField::Payload).await,
            serde_json::to_vec(&original.record).unwrap()
        );
    }

    let resumed =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert_eq!(resumed.status.pending_records, 2);
    assert_eq!(resumed.processed, 0);
}

#[tokio::test]
async fn impossible_critical_defers_then_expires_exactly_while_telemetry_advances() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, spool) = fixture.open();
    let future = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    let mut successor = record("after-bound", Durability::Critical);
    successor.committed_at = future;
    let mut telemetry = record("telemetry-survives", Durability::BestEffortTelemetry);
    telemetry.committed_at = future;
    commit(
        &source,
        vec![
            text_record("beyond-bound", "x".repeat(2 * 1024 * 1024)),
            successor,
            telemetry,
        ],
    )
    .await;

    let first =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert!(first.backpressured);
    assert_eq!(first.status.critical_high_water, 2);
    assert!(first.status.critical.is_none());
    assert_eq!(first.status.telemetry.as_ref().unwrap().sequence, 1);
    assert_eq!(first.status.pending_records, 1);
    assert!(first.status.gaps.is_empty());
    assert!(!first.status.incomplete);
    let namespace = ExportSpoolNamespace {
        destination: destination(),
        provider_kind: "postgresql".into(),
        source_generation: first.status.source_generation.clone(),
    };
    assert_eq!(
        pending_ids(&spool, &namespace).await,
        vec!["telemetry-survives"]
    );

    drop(spool);
    let spool = fixture.open().1;
    let deferred =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert!(deferred.backpressured);
    assert_eq!(deferred.status.critical_high_water, 2);
    assert!(deferred.status.critical.is_none());
    assert_eq!(deferred.status.pending_records, 1);

    source
        .maintain_storage_retention(UtcTimestamp::new(
            instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
        ))
        .await
        .unwrap();
    let recovered =
        ExportIngestor::ingest_once(&source, &spool, destination(), "postgresql", &budget())
            .await
            .unwrap();
    assert!(!recovered.backpressured);
    assert_eq!(recovered.status.critical.as_ref().unwrap().sequence, 2);
    assert_eq!(recovered.status.gaps.len(), 1);
    assert_eq!(
        (
            recovered.status.gaps[0].first,
            recovered.status.gaps[0].last,
            recovered.status.gaps[0].reason
        ),
        (1, 1, ExportGapReason::SourceExpired)
    );
    assert!(recovered.status.incomplete);
    assert_eq!(
        pending_ids(&spool, &namespace).await,
        vec!["after-bound", "telemetry-survives"]
    );
}

#[tokio::test]
async fn expiry_after_first_append_rolls_back_partial_record_and_copies_successor() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let (source, inner) = fixture.open_with_size(8 * 1024 * 1024);
    let source = Arc::new(source);
    let future = UtcTimestamp::new(instant().into_inner() + Duration::days(1));
    let mut successor = record("retained-successor", Durability::Critical);
    successor.committed_at = future;
    commit(
        source.as_ref(),
        vec![
            text_record("expires-in-transfer", "z".repeat(2 * 1024 * 1024)),
            successor,
        ],
    )
    .await;
    let spool = ExpiringSpool {
        inner,
        source: Arc::clone(&source),
        expired: Arc::new(AtomicBool::new(false)),
        now: UtcTimestamp::new(
            instant().into_inner() + Duration::seconds(OPERATIONAL_HISTORY_RETENTION_SECONDS + 1),
        ),
    };

    let result = ExportIngestor::ingest_once(
        source.as_ref(),
        &spool,
        destination(),
        "postgresql",
        &budget(),
    )
    .await
    .unwrap();
    assert!(spool.expired.load(Ordering::Acquire));
    assert_eq!(result.status.critical.as_ref().unwrap().sequence, 2);
    assert_eq!(result.status.pending_records, 1);
    assert_eq!(result.status.gaps.len(), 1);
    assert_eq!(
        (
            result.status.gaps[0].first,
            result.status.gaps[0].last,
            result.status.gaps[0].reason,
        ),
        (1, 1, ExportGapReason::SourceExpired)
    );
    let namespace = ExportSpoolNamespace {
        destination: destination(),
        provider_kind: "postgresql".into(),
        source_generation: result.status.source_generation.clone(),
    };
    assert_eq!(
        pending_ids(&spool.inner, &namespace).await,
        vec!["retained-successor"]
    );
    drop(spool);
    let reopened = fixture.open_with_size(8 * 1024 * 1024).1;
    assert_eq!(
        reopened.status(namespace.clone()).await.unwrap(),
        result.status
    );
    assert_eq!(
        pending_ids(&reopened, &namespace).await,
        vec!["retained-successor"]
    );
}
