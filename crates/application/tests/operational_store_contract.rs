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
    AuthorizationState, CommandAdmissionOutcome, CommittedRecord, CommittedRecordCursor,
    CommittedRecordId, CommittedRecordQuery, DeliveryId, Durability, OperationalStore, Page,
    PageLimit, PendingDelivery, RecoveryBatch, RecoveryQuery, RetainedEventCursor,
    RetainedEventPage, RetainedEventQuery, SnapshotCursor, SnapshotQuery, StorageAdmissionState,
    StorageError, StorageErrorCode, StorageFuture, StorageRetentionStatus,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, Command, CommandOperation, CommandRequest, Connectivity,
    ContractVersion, EventEnvelope, EventId, EventOrigin, EventType, ExternalCommand, PrincipalId,
    ProcessInstanceId, ReleaseId, RequestId, ResourceCapabilities, ResourceRef, RuntimeIdentity,
    StationId, StationSnapshot, TargetInstanceId, UtcTimestamp,
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
    deliveries: Vec<PendingDelivery<TestDeliveryPayload>>,
    records: Vec<CommittedRecord<TestCommittedPayload>>,
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
    ) -> StorageFuture<'_, Page<CommittedRecord<TestCommittedPayload>, CommittedRecordCursor>> {
        Box::pin(async move {
            if query.after.is_some() {
                return Err(StorageError::new(
                    StorageErrorCode::CursorExpired,
                    "committed-record cursor expired after retention",
                ));
            }
            let guard = self.state.lock().expect("memory state");
            let items = guard
                .records
                .iter()
                .filter(|record| {
                    query.include_best_effort_telemetry || record.durability == Durability::Critical
                })
                .take(usize::from(query.limit.get()))
                .cloned()
                .collect();
            Ok(Page {
                items,
                next_cursor: None,
            })
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

#[path = "operational_store_contract/replacement.rs"]
mod replacement;
use replacement::ReplacementMemoryStore;

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

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-1"),
        station_id: text(StationId::new, "station-1"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn command() -> Command<TestCommandPayload> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: text(RequestId::new, "request-1"),
            correlation_id: None,
            resource: resource(),
            operation: CommandOperation::Start {
                authorization_reference: None,
            },
            expires_at: timestamp(9),
        },
        AuthenticatedCommandOrigin::Management {
            principal_id: text(PrincipalId::new, "operator-1"),
        },
    )
    .admit(timestamp(0))
}

fn snapshot() -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: resource(),
        observed_at: timestamp(0),
        connectivity: Connectivity::Connected {
            protocol: uob_contracts::ProtocolEdition::Ocpp16j,
            connected_at: timestamp(0),
            last_message_at: Some(timestamp(0)),
        },
        capabilities: ResourceCapabilities::default(),
        resources: Vec::new(),
        transactions: Vec::new(),
        current_values: Vec::new(),
    }
}

fn event() -> EventEnvelope<TestEventPayload> {
    EventEnvelope {
        event_id: text(EventId::new, "event-1"),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: RuntimeIdentity {
            environment: uob_contracts::Environment::Demo,
            release_id: text(ReleaseId::new, "release-1"),
            release_digest: text(uob_contracts::ArtifactDigest::new, "sha256:abc"),
            process_instance_id: text(ProcessInstanceId::new, "process-1"),
        },
        resource: resource(),
        source_time: None,
        observed_at: timestamp(0),
        event_type: text(EventType::new, "command.admitted.v1"),
        origin: EventOrigin::Management,
        sequence: 1,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: "admitted".to_owned(),
    }
}

fn populated_write()
-> AtomicStoreWrite<TestCommandPayload, TestEventPayload, TestDeliveryPayload, TestCommittedPayload>
{
    AtomicStoreWrite {
        purpose: uob_application::StorageWritePurpose::Routine,
        station_snapshot: Some(snapshot()),
        authorization_changes: vec![AuthorizationChange {
            reference: text(AuthorizationReference::new, "local-auth-1"),
            resource: resource(),
            state: AuthorizationState::Active,
            revision: 1,
            changed_at: timestamp(0),
        }],
        command: Some(command()),
        command_result: None,
        journal_events: vec![event()],
        required_deliveries: vec![PendingDelivery {
            delivery_id: text(DeliveryId::new, "delivery-1"),
            event_id: text(EventId::new, "event-1"),
            target_instance_id: text(TargetInstanceId::new, "target-main"),
            target_configuration_revision: 4,
            ordering_key: resource(),
            deadline: timestamp(8),
            durability: Durability::Critical,
            payload: "event delivery".to_owned(),
        }],
        committed_records: vec![
            CommittedRecord {
                record_id: text(CommittedRecordId::new, "record-1"),
                durability: Durability::Critical,
                committed_at: timestamp(0),
                record: "export event".to_owned(),
            },
            CommittedRecord {
                record_id: text(CommittedRecordId::new, "record-telemetry-1"),
                durability: Durability::BestEffortTelemetry,
                committed_at: timestamp(0),
                record: "export telemetry".to_owned(),
            },
        ],
    }
}

fn assert_store_is_replaceable(
    store: &dyn OperationalStore<
        TestCommandPayload,
        TestEventPayload,
        TestDeliveryPayload,
        TestCommittedPayload,
    >,
) {
    let page = block_on(store.read_snapshots(SnapshotQuery {
        after: None,
        limit: PageLimit::new(1).expect("bounded page"),
    }))
    .expect("snapshot read");
    assert!(page.items.len() <= 1);
}

#[path = "operational_store_contract/cases.rs"]
mod cases;
