use uob_contracts::{
    Command, CommandResult, EventEnvelope, RequestId, StationSnapshot, UtcTimestamp,
};

use super::{
    AtomicStoreWrite, AtomicWriteOutcome, CommittedRecord, CommittedRecordCursor,
    CommittedRecordQuery, DeliveryAttempt, DeliveryId, Page, PageLimit, PendingDeliveryQuery,
    RecordedDeliveryAttempt, RecoveryBatch, RecoveryQuery, RetainedEventCursor, RetainedEventQuery,
    ScheduledDelivery, SnapshotCursor, SnapshotQuery, StorageFuture,
};

/// Application-owned authoritative persistence and committed-record read ports.
///
/// The type parameters keep evolving canonical payloads typed while allowing callers to use
/// `dyn OperationalStore<C, E, D, R>`. Methods deliberately expose no driver connection,
/// statement, or physical-layout types.
pub trait OperationalStore<C, E, D, R>: Send + Sync {
    /// Atomically persists one application operation and performs command deduplication.
    /// An identical duplicate returns `CommandAdmissionOutcome::Duplicate` without applying
    /// the bundled state, journal, delivery, or committed-record mutations again. Reusing a
    /// request ID for a different command returns `StorageErrorCode::Conflict`.
    fn write_atomic(
        &self,
        write: AtomicStoreWrite<C, E, D, R>,
    ) -> StorageFuture<'_, AtomicWriteOutcome>;

    /// Reads a bounded page of latest station snapshots.
    fn read_snapshots(
        &self,
        query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>>;

    /// Reads a bounded page from one retained durable event stream.
    fn read_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> StorageFuture<'_, Page<EventEnvelope<E>, RetainedEventCursor>>;

    /// Reads records that became visible through completed atomic writes.
    fn read_committed_records(
        &self,
        query: CommittedRecordQuery,
    ) -> StorageFuture<'_, Page<CommittedRecord<R>, CommittedRecordCursor>>;

    /// Reads a bounded recovery view without granting unrestricted storage access.
    fn recover(&self, query: RecoveryQuery) -> StorageFuture<'_, RecoveryBatch<C, D>>;

    /// Reads one durably admitted command by idempotency identity.
    fn command_by_request_id(&self, request_id: RequestId)
    -> StorageFuture<'_, Option<Command<C>>>;

    /// Reads the latest durable lifecycle result for one command request.
    fn command_result_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<CommandResult>>;

    /// Removes resolved command identities whose seven-day retention period has elapsed.
    ///
    /// Commands without a terminal result, including transmission-uncertain commands, must be
    /// retained regardless of age. The returned count is the number of removed identities.
    fn prune_command_deduplication(&self, now: UtcTimestamp) -> StorageFuture<'_, u64>;
}

/// Narrow durable outbox surface used by target delivery supervision.
///
/// Keeping this separate from [`OperationalStore`] prevents the target worker from gaining
/// command, authorization, snapshot, journal, or external-export access.
pub trait TargetDeliveryStore<D>: Send + Sync {
    /// Reads ready work in stable insertion order while preserving head-of-line ordering for each
    /// canonical resource key.
    fn read_pending_deliveries(
        &self,
        query: PendingDeliveryQuery,
    ) -> StorageFuture<'_, Vec<ScheduledDelivery<D>>>;

    /// Persists the exact report and its retry/final decision in one transaction.
    fn record_delivery_attempt(&self, attempt: DeliveryAttempt) -> StorageFuture<'_, ()>;

    /// Reads the bounded audit history for one stable delivery identity, newest first.
    fn delivery_attempts(
        &self,
        delivery_id: DeliveryId,
        limit: PageLimit,
    ) -> StorageFuture<'_, Vec<RecordedDeliveryAttempt>>;
}
