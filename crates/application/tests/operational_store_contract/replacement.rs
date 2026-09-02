use super::*;

#[derive(Clone, Default)]
pub(super) struct ReplacementMemoryStore(MemoryStore);

impl
    OperationalStore<
        TestCommandPayload,
        TestEventPayload,
        TestDeliveryPayload,
        TestCommittedPayload,
    > for ReplacementMemoryStore
{
    fn write_atomic(
        &self,
        write: AtomicStoreWrite<
            TestCommandPayload,
            TestEventPayload,
            TestDeliveryPayload,
            TestCommittedPayload,
        >,
    ) -> StorageFuture<'_, AtomicWriteOutcome> {
        self.0.write_atomic(write)
    }

    fn read_snapshots(
        &self,
        query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        self.0.read_snapshots(query)
    }

    fn read_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> StorageFuture<'_, RetainedEventPage<TestEventPayload>> {
        self.0.read_retained_events(query)
    }

    fn read_committed_records(
        &self,
        query: CommittedRecordQuery,
    ) -> StorageFuture<'_, Page<CommittedRecord<TestCommittedPayload>, CommittedRecordCursor>> {
        self.0.read_committed_records(query)
    }

    fn recover(
        &self,
        query: RecoveryQuery,
    ) -> StorageFuture<'_, RecoveryBatch<TestCommandPayload, TestDeliveryPayload>> {
        self.0.recover(query)
    }

    fn command_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<Command<TestCommandPayload>>> {
        self.0.command_by_request_id(request_id)
    }

    fn command_result_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<uob_contracts::CommandResult>> {
        self.0.command_result_by_request_id(request_id)
    }

    fn prune_command_deduplication(&self, now: UtcTimestamp) -> StorageFuture<'_, u64> {
        self.0.prune_command_deduplication(now)
    }

    fn maintain_storage_retention(
        &self,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, StorageRetentionStatus> {
        self.0.maintain_storage_retention(now)
    }

    fn storage_retention_status(&self) -> StorageFuture<'_, StorageRetentionStatus> {
        self.0.storage_retention_status()
    }
}
