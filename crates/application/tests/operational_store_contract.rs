use std::{
    collections::BTreeMap,
    future::Future,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, AtomicWriteOutcome, AuthorizationChange, AuthorizationReference,
    AuthorizationState, BudgetedRecordChunk, CommandAdmissionOutcome, CommandHistoryCursor,
    CommandHistoryQuery, CommandHistoryScope, CommittedRecord, CommittedRecordChunkQuery,
    CommittedRecordChunkResult, CommittedRecordCursor, CommittedRecordDescriptor,
    CommittedRecordField, CommittedRecordId, CommittedRecordPage, CommittedRecordQuery,
    CommittedRecordReadToken, DeliveryId, Durability, OperationalStore, Page, PageLimit,
    PendingDelivery, RecoveryBatch, RecoveryQuery, RetainedEventCursor, RetainedEventPage,
    RetainedEventQuery, RuntimeResourceBudget, RuntimeResourceLimits, SnapshotCursor,
    SnapshotQuery, StorageAdmissionState, StorageError, StorageErrorCode, StorageFuture,
    StorageRetentionStatus, WorkClass,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, Command, CommandOperation, CommandRequest,
    CommandSummary, Connectivity, ContractVersion, EventEnvelope, EventId, EventOrigin, EventType,
    ExternalCommand, PrincipalId, ProcessInstanceId, ReleaseId, RequestId, ResourceCapabilities,
    ResourceRef, RuntimeIdentity, StationId, StationSnapshot, TargetInstanceId, UtcTimestamp,
};

type TestCommandPayload = String;
type TestEventPayload = String;
type TestDeliveryPayload = String;
type TestCommittedPayload = String;

#[derive(Clone, Default)]
struct MemoryState {
    commands: BTreeMap<String, Command<TestCommandPayload>>,
    snapshots: Vec<StationSnapshot>,
    authorization: Vec<AuthorizationChange>,
    events: Vec<EventEnvelope<TestEventPayload>>,
    command_results: Vec<uob_contracts::CommandResult>,
    history_cursors: BTreeMap<String, (ResourceRef, CommandHistoryScope, RequestId)>,
    next_history_cursor: u64,
    deliveries: Vec<PendingDelivery<TestDeliveryPayload>>,
    records: Vec<CommittedRecord<TestCommittedPayload>>,
    next_event_sequence: u64,
}

#[derive(Clone, Default)]
struct MemoryStore {
    state: Arc<Mutex<MemoryState>>,
    fail_next_write: Arc<Mutex<bool>>,
}

impl MemoryStore {
    fn fail_next_write(&self) {
        *self.fail_next_write.lock().expect("failure flag") = true;
    }
}

impl
    OperationalStore<
        TestCommandPayload,
        TestEventPayload,
        TestDeliveryPayload,
        TestCommittedPayload,
    > for MemoryStore
{
    fn reserve_event_sequence(&self) -> StorageFuture<'_, u64> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("memory state");
            state.next_event_sequence += 1;
            Ok(state.next_event_sequence)
        })
    }

    fn write_atomic(
        &self,
        write: AtomicStoreWrite<
            TestCommandPayload,
            TestEventPayload,
            TestDeliveryPayload,
            TestCommittedPayload,
        >,
    ) -> StorageFuture<'_, AtomicWriteOutcome> {
        Box::pin(async move {
            let mut guard = self.state.lock().expect("memory state");
            let mut candidate = guard.clone();
            let command_outcome = if let Some(command) = write.command {
                match candidate.commands.get(command.request_id.as_str()) {
                    Some(existing) if existing == &command => {
                        return Ok(AtomicWriteOutcome {
                            command: Some(CommandAdmissionOutcome::Duplicate { result: None }),
                        });
                    }
                    Some(_) => {
                        return Err(StorageError::new(
                            StorageErrorCode::Conflict,
                            "request ID is already associated with another command",
                        ));
                    }
                    None => {
                        candidate
                            .commands
                            .insert(command.request_id.as_str().to_owned(), command);
                        Some(CommandAdmissionOutcome::Admitted)
                    }
                }
            } else {
                None
            };

            candidate.snapshots.extend(write.station_snapshot);
            candidate.authorization.extend(write.authorization_changes);
            candidate.events.extend(write.journal_events);
            candidate.command_results.extend(write.command_result);
            candidate.deliveries.extend(write.required_deliveries);
            candidate.records.extend(write.committed_records);

            let mut fail = self.fail_next_write.lock().expect("failure flag");
            if *fail {
                *fail = false;
                return Err(StorageError::new(
                    StorageErrorCode::Unavailable,
                    "injected commit failure",
                ));
            }

            *guard = candidate;
            Ok(AtomicWriteOutcome {
                command: command_outcome,
            })
        })
    }

    fn read_snapshots(
        &self,
        query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        Box::pin(async move {
            if query.after.is_some() {
                return Err(StorageError::new(
                    StorageErrorCode::CursorExpired,
                    "snapshot cursor is outside retained state",
                ));
            }
            let guard = self.state.lock().expect("memory state");
            let items = guard
                .snapshots
                .iter()
                .take(usize::from(query.limit.get()))
                .cloned()
                .collect();
            Ok(Page {
                items,
                next_cursor: None,
            })
        })
    }

    fn station_snapshot(&self, station: ResourceRef) -> StorageFuture<'_, Option<StationSnapshot>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .expect("memory state")
                .snapshots
                .iter()
                .rev()
                .find(|snapshot| snapshot.station == station)
                .cloned())
        })
    }

    fn read_scoped_snapshots(
        &self,
        query: SnapshotQuery,
        stations: Vec<ResourceRef>,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        Box::pin(async move {
            let mut values = self
                .state
                .lock()
                .expect("memory state")
                .snapshots
                .iter()
                .filter(|snapshot| stations.contains(&snapshot.station))
                .cloned()
                .collect::<Vec<_>>();
            values.sort_by_key(|snapshot| serde_json::to_string(&snapshot.station).unwrap());
            values.dedup_by(|left, right| left.station == right.station);
            if let Some(after) = query.after {
                values.retain(|snapshot| {
                    serde_json::to_string(&snapshot.station).unwrap().as_str() > after.as_str()
                });
            }
            let has_more = values.len() > usize::from(query.limit.get());
            values.truncate(usize::from(query.limit.get()));
            let next_cursor = if has_more {
                values.last().map(|snapshot| {
                    SnapshotCursor::new(serde_json::to_string(&snapshot.station).unwrap()).unwrap()
                })
            } else {
                None
            };
            Ok(Page {
                items: values,
                next_cursor,
            })
        })
    }

    fn read_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> StorageFuture<'_, RetainedEventPage<TestEventPayload>> {
        Box::pin(async move {
            if query.after.is_some() {
                return Err(StorageError::new(
                    StorageErrorCode::CursorExpired,
                    "event cursor expired after retention",
                ));
            }
            let guard = self.state.lock().expect("memory state");
            let items = guard
                .events
                .iter()
                .filter(|event| event.resource == query.resource)
                .take(usize::from(query.limit.get()))
                .cloned()
                .collect();
            Ok(RetainedEventPage {
                events: items,
                resume_cursor: None,
                has_more: false,
            })
        })
    }

    fn read_committed_records(
        &self,
        query: CommittedRecordQuery,
        _budget: &RuntimeResourceBudget,
    ) -> StorageFuture<'_, CommittedRecordPage> {
        Box::pin(async move {
            if query.after.is_some() {
                return Err(StorageError::new(
                    StorageErrorCode::CursorExpired,
                    "committed-record cursor expired after retention",
                ));
            }
            let guard = self.state.lock().expect("memory state");
            let records: Vec<_> = guard
                .records
                .iter()
                .filter(|record| record.durability == query.durability)
                .collect();
            let high_water = records.len() as u64;
            let items = records
                .into_iter()
                .enumerate()
                .take(usize::from(query.limit.get()))
                .map(|(index, value)| {
                    let sequence = index as u64 + 1;
                    let lengths = [
                        value.record_id.as_str().len() as u64,
                        serde_json::to_string(&value.committed_at).unwrap().len() as u64,
                        serde_json::to_string(&value.record).unwrap().len() as u64,
                    ];
                    CommittedRecordDescriptor {
                        token: CommittedRecordReadToken::new(
                            "mock".into(),
                            query.durability,
                            sequence,
                            i64::try_from(sequence).expect("memory record sequence fits i64"),
                            lengths,
                            0,
                        ),
                        durability: query.durability,
                        sequence,
                        cursor: CommittedRecordCursor::new(format!("mock:{sequence}")).unwrap(),
                        record_id_len: lengths[0],
                        committed_at_len: lengths[1],
                        payload_len: lengths[2],
                    }
                })
                .collect::<Vec<_>>();
            Ok(CommittedRecordPage {
                has_more: high_water > items.len() as u64,
                items,
                source_generation: "mock".into(),
                resume_cursor: CommittedRecordCursor::new(format!("mock:{high_water}"))?,
                high_water,
                expired_prefix: 0,
                lost_records: 0,
                legacy_baseline_incomplete: false,
                reservation: None,
            })
        })
    }

    fn read_committed_record_chunk(
        &self,
        query: CommittedRecordChunkQuery,
        budget: &RuntimeResourceBudget,
    ) -> StorageFuture<'_, CommittedRecordChunkResult> {
        let reservation = budget.try_reserve(WorkClass::ExporterBatch, query.max_bytes);
        Box::pin(async move {
            let reservation = reservation.map_err(|_| {
                StorageError::new(StorageErrorCode::Busy, "mock chunk admission unavailable")
            })?;
            let guard = self.state.lock().expect("memory state");
            let index =
                usize::try_from(query.token.sequence().saturating_sub(1)).map_err(|_| {
                    StorageError::new(StorageErrorCode::InvalidRequest, "unknown record")
                })?;
            let value = guard
                .records
                .iter()
                .filter(|value| value.durability == query.token.durability())
                .nth(index)
                .ok_or_else(|| {
                    StorageError::new(StorageErrorCode::InvalidRequest, "unknown record")
                })?;
            let bytes = match query.field {
                CommittedRecordField::RecordId => value.record_id.as_str().as_bytes().to_vec(),
                CommittedRecordField::CommittedAt => {
                    serde_json::to_vec(&value.committed_at).unwrap()
                }
                CommittedRecordField::Payload => serde_json::to_vec(&value.record).unwrap(),
            };
            let offset = usize::try_from(query.offset).map_err(|_| {
                StorageError::new(StorageErrorCode::InvalidRequest, "invalid offset")
            })?;
            let part = bytes
                .get(offset..)
                .ok_or_else(|| {
                    StorageError::new(StorageErrorCode::InvalidRequest, "invalid offset")
                })?
                .iter()
                .take(query.max_bytes)
                .copied()
                .collect();
            Ok(CommittedRecordChunkResult::Data(BudgetedRecordChunk::new(
                query.field,
                query.offset,
                part,
                bytes.len() as u64,
                reservation,
            )?))
        })
    }

    fn recover(
        &self,
        query: RecoveryQuery,
    ) -> StorageFuture<'_, RecoveryBatch<TestCommandPayload, TestDeliveryPayload>> {
        Box::pin(async move {
            let guard = self.state.lock().expect("memory state");
            let limit = usize::from(query.limit.get());
            Ok(RecoveryBatch {
                station_snapshots: guard.snapshots.clone(),
                authorization: guard.authorization.clone(),
                active_commands: guard.commands.values().take(limit).cloned().collect(),
                command_results: guard.command_results.iter().take(limit).cloned().collect(),
                pending_deliveries: guard.deliveries.iter().take(limit).cloned().collect(),
                has_more: guard.commands.len().max(guard.deliveries.len()) > limit,
            })
        })
    }

    fn command_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<Command<TestCommandPayload>>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .expect("memory state")
                .commands
                .get(request_id.as_str())
                .cloned())
        })
    }

    fn command_result_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<uob_contracts::CommandResult>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .expect("memory state")
                .command_results
                .iter()
                .rev()
                .find(|result| result.return_route.request_id == request_id)
                .cloned())
        })
    }
    fn read_command_history(
        &self,
        query: CommandHistoryQuery,
        scope: CommandHistoryScope,
    ) -> StorageFuture<'_, Page<CommandSummary, CommandHistoryCursor>> {
        Box::pin(async move { self.history_page(&query, scope) })
    }

    fn prune_command_deduplication(&self, _now: UtcTimestamp) -> StorageFuture<'_, u64> {
        Box::pin(async { Ok(0) })
    }

    fn maintain_storage_retention(
        &self,
        _now: UtcTimestamp,
    ) -> StorageFuture<'_, StorageRetentionStatus> {
        Box::pin(async { Ok(empty_retention_status()) })
    }

    fn storage_retention_status(&self) -> StorageFuture<'_, StorageRetentionStatus> {
        Box::pin(async { Ok(empty_retention_status()) })
    }
}

#[path = "operational_store_contract/history.rs"]
mod history;

#[path = "operational_store_contract/replacement.rs"]
mod replacement;
use replacement::{ReplacementMemoryStore, assert_store_is_replaceable};

fn empty_retention_status() -> StorageRetentionStatus {
    StorageRetentionStatus {
        budget_bytes: 1024,
        active_session_reserve_bytes: 128,
        used_bytes: 0,
        new_session_admission: StorageAdmissionState::Available,
        retained_critical_events: 0,
        retained_required_deliveries: 0,
        dropped_best_effort_telemetry: 0,
        dropped_best_effort_deliveries: 0,
        pruned_expired_events: 0,
        pruned_expired_delivery_attempts: 0,
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid test identity")
}

fn timestamp(minute: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).expect("fixture date"),
            Time::from_hms(15, minute, 0).expect("fixture time"),
        )
        .assume_offset(UtcOffset::UTC),
    )
}

#[path = "operational_store_contract/fixtures.rs"]
mod fixtures;
use fixtures::{command, populated_write, resource};

#[path = "operational_store_contract/cases.rs"]
mod cases;
