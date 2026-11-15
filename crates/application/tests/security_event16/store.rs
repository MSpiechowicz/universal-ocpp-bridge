//! Recording store: `write_atomic` and the security event dedup read are real, answered from the
//! journal it recorded; every other port reports itself unsupported.
use std::sync::Mutex;
use uob_application::{
    AtomicStoreWrite, AtomicWriteOutcome, CommittedRecordChunkQuery, CommittedRecordChunkResult,
    CommittedRecordPage, CommittedRecordQuery, OperationalStore, Page, RecoveryBatch,
    RecoveryQuery, RetainedEventPage, RetainedEventQuery, RuntimeResourceBudget,
    SecurityEventKey16, SecurityEventStore16, SnapshotCursor, SnapshotQuery, StationEvent,
    StorageError, StorageErrorCode, StorageFuture, StorageRetentionStatus,
};
use uob_contracts::{
    Command, CommandResult, CommandSummary, EventEnvelope, RequestId, ResourceRef, StationSnapshot,
    TransactionSnapshot, UtcTimestamp,
};

#[derive(Default)]
pub struct RecordingStore {
    pub writes: Mutex<Vec<AtomicStoreWrite<String, StationEvent, TransactionSnapshot, String>>>,
    pub fail: Mutex<bool>,
}

impl RecordingStore {
    pub fn journal(&self) -> Vec<EventEnvelope<StationEvent>> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .flat_map(|write| write.journal_events.clone())
            .collect()
    }
}

impl SecurityEventStore16 for RecordingStore {
    fn security_event_recorded_16(&self, key: SecurityEventKey16) -> StorageFuture<'_, bool> {
        let recorded = self.journal().iter().any(|event| {
            matches!(
                &event.payload,
                StationEvent::SecurityEvent16 { security_event_16, .. }
                    if event.resource == key.station
                        && security_event_16.event_type == key.event_type
                        && security_event_16.timestamp == key.timestamp
                        && security_event_16.tech_info == key.tech_info
            )
        });

        Box::pin(async move { Ok(recorded) })
    }
}

fn unsupported<T: Send + 'static>() -> StorageFuture<'static, T> {
    Box::pin(async {
        Err(StorageError::new(
            StorageErrorCode::Unavailable,
            "recording store supports only atomic writes",
        ))
    })
}

impl OperationalStore<String, StationEvent, TransactionSnapshot, String> for RecordingStore {
    fn reserve_event_sequence(&self) -> StorageFuture<'_, u64> {
        unsupported()
    }

    fn write_atomic(
        &self,
        write: AtomicStoreWrite<String, StationEvent, TransactionSnapshot, String>,
    ) -> StorageFuture<'_, AtomicWriteOutcome> {
        Box::pin(async move {
            if *self.fail.lock().unwrap() {
                return Err(StorageError::new(
                    StorageErrorCode::Unavailable,
                    "injected write failure",
                ));
            }
            self.writes.lock().unwrap().push(write);
            Ok(AtomicWriteOutcome { command: None })
        })
    }

    fn read_snapshots(
        &self,
        _query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        unsupported()
    }

    fn station_snapshot(&self, _: ResourceRef) -> StorageFuture<'_, Option<StationSnapshot>> {
        unsupported()
    }

    fn read_scoped_snapshots(
        &self,
        _query: SnapshotQuery,
        _stations: Vec<ResourceRef>,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        unsupported()
    }

    fn read_retained_events(
        &self,
        _query: RetainedEventQuery,
    ) -> StorageFuture<'_, RetainedEventPage<StationEvent>> {
        unsupported()
    }

    fn read_committed_records(
        &self,
        _query: CommittedRecordQuery,
        _budget: &RuntimeResourceBudget,
    ) -> StorageFuture<'_, CommittedRecordPage> {
        unsupported()
    }

    fn read_committed_record_chunk(
        &self,
        _query: CommittedRecordChunkQuery,
        _budget: &RuntimeResourceBudget,
    ) -> StorageFuture<'_, CommittedRecordChunkResult> {
        unsupported()
    }

    fn recover(
        &self,
        _query: RecoveryQuery,
    ) -> StorageFuture<'_, RecoveryBatch<String, TransactionSnapshot>> {
        unsupported()
    }

    fn command_by_request_id(&self, _: RequestId) -> StorageFuture<'_, Option<Command<String>>> {
        unsupported()
    }

    fn command_result_by_request_id(
        &self,
        _: RequestId,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        unsupported()
    }

    fn read_command_history(
        &self,
        _query: uob_application::CommandHistoryQuery,
        _scope: uob_application::CommandHistoryScope,
    ) -> StorageFuture<'_, Page<CommandSummary, uob_application::CommandHistoryCursor>> {
        unsupported()
    }

    fn prune_command_deduplication(&self, _now: UtcTimestamp) -> StorageFuture<'_, u64> {
        unsupported()
    }

    fn maintain_storage_retention(
        &self,
        _now: UtcTimestamp,
    ) -> StorageFuture<'_, StorageRetentionStatus> {
        unsupported()
    }

    fn storage_retention_status(&self) -> StorageFuture<'_, StorageRetentionStatus> {
        unsupported()
    }
}
