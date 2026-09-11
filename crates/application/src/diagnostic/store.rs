//! Scoped persistence observer; delegates all business decisions to the authoritative store.
use crate::{
    AtomicStoreWrite, AtomicWriteOutcome, CommandAdmissionOutcome, CommittedRecord,
    CommittedRecordCursor, CommittedRecordQuery, FlowEvidence, FlowSpan, FlowStage,
    OperationalStore, Page, RecoveryBatch, RecoveryQuery, RetainedEventPage, RetainedEventQuery,
    SnapshotCursor, SnapshotQuery, StorageFuture, StorageRetentionStatus,
};
use uob_contracts::{Command, CommandResult, RequestId, StationSnapshot, UtcTimestamp};
/// Borrowed store adapter tying one ordered operation to its actual commit completion.
pub struct DiagnosticStore<'a, C, E, D, R> {
    inner: &'a dyn OperationalStore<C, E, D, R>,
    trace: FlowSpan,
    committed: std::sync::atomic::AtomicBool,
}
impl<'a, C, E, D, R> DiagnosticStore<'a, C, E, D, R> {
    /// Creates a per-operation observer, without creating a second storage owner.
    pub fn new(inner: &'a dyn OperationalStore<C, E, D, R>, trace: FlowSpan) -> Self {
        Self {
            inner,
            trace,
            committed: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Whether this operation actually completed an authoritative write.
    #[must_use]
    pub fn committed(&self) -> bool {
        self.committed.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl<C: Send + 'static, E: Send + 'static, D: Send + 'static, R: Send + 'static>
    OperationalStore<C, E, D, R> for DiagnosticStore<'_, C, E, D, R>
{
    fn reserve_transaction_id(&self) -> StorageFuture<'_, i32> {
        self.inner.reserve_transaction_id()
    }

    fn write_atomic(
        &self,
        write: AtomicStoreWrite<C, E, D, R>,
    ) -> StorageFuture<'_, AtomicWriteOutcome> {
        Box::pin(async move {
            let result = self.inner.write_atomic(write).await;
            let evidence = match &result {
                Ok(outcome)
                    if matches!(
                        outcome.command,
                        Some(CommandAdmissionOutcome::Duplicate { .. })
                    ) =>
                {
                    FlowEvidence::Duplicate
                }
                Ok(_) => FlowEvidence::Completed,
                Err(_) => FlowEvidence::Failed,
            };
            if result.is_ok() {
                self.committed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.trace.emit(FlowStage::DurableCommit, evidence);
            result
        })
    }
    fn read_snapshots(
        &self,
        query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        self.inner.read_snapshots(query)
    }
    fn read_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> StorageFuture<'_, RetainedEventPage<E>> {
        self.inner.read_retained_events(query)
    }
    fn read_committed_records(
        &self,
        query: CommittedRecordQuery,
    ) -> StorageFuture<'_, Page<CommittedRecord<R>, CommittedRecordCursor>> {
        self.inner.read_committed_records(query)
    }
    fn recover(&self, query: RecoveryQuery) -> StorageFuture<'_, RecoveryBatch<C, D>> {
        self.inner.recover(query)
    }
    fn command_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<Command<C>>> {
        self.inner.command_by_request_id(request_id)
    }
    fn command_result_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        self.inner.command_result_by_request_id(request_id)
    }
    fn prune_command_deduplication(&self, now: UtcTimestamp) -> StorageFuture<'_, u64> {
        self.inner.prune_command_deduplication(now)
    }
    fn maintain_storage_retention(
        &self,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, StorageRetentionStatus> {
        self.inner.maintain_storage_retention(now)
    }
    fn storage_retention_status(&self) -> StorageFuture<'_, StorageRetentionStatus> {
        self.inner.storage_retention_status()
    }
}
