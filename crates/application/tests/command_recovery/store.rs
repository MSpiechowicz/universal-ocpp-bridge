use super::*;

impl OperationalStore<String, String, String, String> for MemoryStore {
    fn reserve_event_sequence(&self) -> StorageFuture<'_, u64> {
        Box::pin(async {
            Err(uob_application::StorageError::new(
                uob_application::StorageErrorCode::InvalidRequest,
                "command recovery memory store does not implement event reservations",
            ))
        })
    }

    fn write_atomic(
        &self,
        write: AtomicStoreWrite<String, String, String, String>,
    ) -> StorageFuture<'_, AtomicWriteOutcome> {
        Box::pin(async move {
            let mut state = self.0.lock().expect("store state");
            if state.reject_new_starts
                && write.purpose == uob_application::StorageWritePurpose::NewSessionStart
            {
                return Err(uob_application::StorageError::new(
                    uob_application::StorageErrorCode::CapacityExhausted,
                    "critical operational storage capacity cannot be maintained",
                ));
            }
            let outcome = if let Some(command) = write.command {
                if let Some(existing) = state.commands.get(command.request_id.as_str()) {
                    assert_eq!(existing, &command, "test does not submit ID conflicts");
                    return Ok(AtomicWriteOutcome {
                        command: Some(CommandAdmissionOutcome::Duplicate {
                            result: state
                                .results
                                .get(command.request_id.as_str())
                                .cloned()
                                .map(Box::new),
                        }),
                    });
                }
                state
                    .commands
                    .insert(command.request_id.as_str().to_owned(), command);
                Some(CommandAdmissionOutcome::Admitted)
            } else {
                None
            };
            if let Some(result) = write.command_result {
                state
                    .results
                    .insert(result.return_route.request_id.as_str().to_owned(), result);
            }
            Ok(AtomicWriteOutcome { command: outcome })
        })
    }

    fn read_snapshots(
        &self,
        _query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        Box::pin(async {
            Ok(Page {
                items: Vec::new(),
                next_cursor: None,
            })
        })
    }

    fn station_snapshot(
        &self,
        _station: ResourceRef,
    ) -> StorageFuture<'_, Option<StationSnapshot>> {
        Box::pin(async {
            Err(uob_application::StorageError::new(
                uob_application::StorageErrorCode::InvalidRequest,
                "command recovery memory store does not implement station reads",
            ))
        })
    }

    fn read_scoped_snapshots(
        &self,
        _query: SnapshotQuery,
        _stations: Vec<ResourceRef>,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        Box::pin(async {
            Err(uob_application::StorageError::new(
                uob_application::StorageErrorCode::InvalidRequest,
                "command recovery memory store does not implement station reads",
            ))
        })
    }

    fn read_retained_events(
        &self,
        _query: RetainedEventQuery,
    ) -> StorageFuture<'_, RetainedEventPage<String>> {
        Box::pin(async {
            Ok(RetainedEventPage {
                events: Vec::new(),
                resume_cursor: None,
                has_more: false,
            })
        })
    }

    fn read_committed_records(
        &self,
        _query: CommittedRecordQuery,
    ) -> StorageFuture<'_, Page<CommittedRecord<String>, CommittedRecordCursor>> {
        Box::pin(async {
            Ok(Page {
                items: Vec::new(),
                next_cursor: None,
            })
        })
    }

    fn recover(&self, query: RecoveryQuery) -> StorageFuture<'_, RecoveryBatch<String, String>> {
        Box::pin(async move {
            let state = self.0.lock().expect("store state");
            let active = state
                .commands
                .values()
                .filter(|command| {
                    state
                        .results
                        .get(command.request_id.as_str())
                        .is_none_or(|result| {
                            matches!(
                                result.lifecycle,
                                CommandLifecycle::Admitted
                                    | CommandLifecycle::Dispatched
                                    | CommandLifecycle::TransmissionUncertain { .. }
                            )
                        })
                })
                .take(usize::from(query.limit.get()))
                .cloned()
                .collect::<Vec<_>>();
            let results = active
                .iter()
                .filter_map(|command| state.results.get(command.request_id.as_str()).cloned())
                .collect();
            Ok(RecoveryBatch {
                station_snapshots: Vec::new(),
                authorization: Vec::new(),
                active_commands: active,
                command_results: results,
                pending_deliveries: Vec::new(),
                has_more: false,
            })
        })
    }

    fn command_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<Command<String>>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .expect("store state")
                .commands
                .get(request_id.as_str())
                .cloned())
        })
    }

    fn command_result_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .expect("store state")
                .results
                .get(request_id.as_str())
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
        self.storage_retention_status()
    }

    fn storage_retention_status(&self) -> StorageFuture<'_, StorageRetentionStatus> {
        Box::pin(async {
            Ok(StorageRetentionStatus {
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
            })
        })
    }
}
