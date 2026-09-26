use super::*;
use uob_application::COMMAND_HISTORY_CURSOR_PREFIX;
use uob_contracts::{
    CanonicalConnectorId, CanonicalResource, CommandLifecycle, CommandOperationKind, CommandResult,
    CommandReturnRoute, NativeProtocolReference, ObservedCommandEffect,
};

impl MemoryStore {
    pub(super) fn history_page(
        &self,
        query: &CommandHistoryQuery,
        scope: CommandHistoryScope,
    ) -> Result<Page<CommandSummary, CommandHistoryCursor>, StorageError> {
        if query.station.resource.is_some()
            || !matches!(
                query.station.native_protocol_reference,
                None | Some(
                    NativeProtocolReference::Ocpp16 { connector_id: 0 }
                        | NativeProtocolReference::Ocpp201 {
                            evse_id: 0,
                            connector_id: None,
                        }
                )
            )
        {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "history scope must contain a canonical station reference",
            ));
        }
        if scope.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                next_cursor: None,
            });
        }

        let station = ResourceRef {
            bridge_id: query.station.bridge_id.clone(),
            station_id: query.station.station_id.clone(),
            resource: None,
            native_protocol_reference: None,
        };
        let scope = normalized_scope(scope);
        let mut guard = self.state.lock().expect("memory state");
        let anchor = query
            .after
            .as_ref()
            .map(|cursor| {
                let (cursor_station, cursor_scope, request_id) = guard
                    .history_cursors
                    .get(cursor.as_str())
                    .ok_or_else(invalid_cursor)?;
                if cursor_station != &station || cursor_scope != &scope {
                    return Err(invalid_cursor());
                }
                guard
                    .commands
                    .get(request_id.as_str())
                    .map(|command| (command.admitted_at, request_id.as_str()))
                    .ok_or_else(|| {
                        StorageError::new(
                            StorageErrorCode::CursorExpired,
                            "command history cursor expired",
                        )
                    })
            })
            .transpose()?;

        let mut commands = guard
            .commands
            .values()
            .filter(|command| scope.permits(&command.resource, &station))
            .filter(|command| {
                anchor.is_none_or(|(at, id)| {
                    (command.admitted_at, command.request_id.as_str()) < (at, id)
                })
            })
            .collect::<Vec<_>>();
        commands.sort_unstable_by(|left, right| {
            (right.admitted_at, right.request_id.as_str())
                .cmp(&(left.admitted_at, left.request_id.as_str()))
        });
        let has_more = commands.len() > usize::from(query.limit.get());
        commands.truncate(usize::from(query.limit.get()));
        let next_id = has_more.then(|| {
            commands
                .last()
                .expect("nonzero page limit")
                .request_id
                .clone()
        });
        let items = commands
            .into_iter()
            .map(|command| command_summary(command, &guard.command_results))
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = next_id.map(|request_id| {
            guard.next_history_cursor += 1;
            let cursor = CommandHistoryCursor::new(format!(
                "{COMMAND_HISTORY_CURSOR_PREFIX}{}",
                guard.next_history_cursor
            ))
            .expect("valid generated history cursor");
            guard
                .history_cursors
                .insert(cursor.as_str().to_owned(), (station, scope, request_id));
            cursor
        });
        Ok(Page { items, next_cursor })
    }
}

fn command_summary(
    command: &Command<String>,
    results: &[CommandResult],
) -> Result<CommandSummary, StorageError> {
    let result = results
        .iter()
        .rev()
        .find(|result| result.return_route.request_id == command.request_id);
    if result.is_some_and(|result| result.resource != command.resource) {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "command history result mismatch",
        ));
    }
    let operation = match &command.operation {
        CommandOperation::Start { .. } => CommandOperationKind::Start,
        CommandOperation::Stop { .. } => CommandOperationKind::Stop,
        CommandOperation::SetChargingLimit(_) => CommandOperationKind::SetChargingLimit,
        CommandOperation::Ocpp(_) => CommandOperationKind::Ocpp,
    };
    Ok(CommandSummary {
        request_id: command.request_id.clone(),
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        operation,
        admitted_at: command.admitted_at,
        expires_at: command.expires_at,
        lifecycle: result.map(|result| result.lifecycle.clone()),
        recorded_at: result.map(|result| result.recorded_at),
        observed_effects: result.map_or_else(Vec::new, |result| result.observed_effects.clone()),
    })
}

fn normalized_scope(mut scope: CommandHistoryScope) -> CommandHistoryScope {
    fn key(resource: &uob_contracts::CanonicalResource) -> (u8, &str, Option<&str>) {
        match resource {
            uob_contracts::CanonicalResource::Connector { connector_id } => {
                (0, connector_id.as_str(), None)
            }
            uob_contracts::CanonicalResource::Evse {
                evse_id,
                connector_id,
            } => (
                1,
                evse_id.as_str(),
                connector_id.as_ref().map(CanonicalConnectorId::as_str),
            ),
        }
    }

    scope
        .resources
        .sort_unstable_by(|left, right| key(left).cmp(&key(right)));
    scope.resources.dedup();
    scope
}

fn invalid_cursor() -> StorageError {
    StorageError::new(
        StorageErrorCode::InvalidRequest,
        "invalid command history cursor",
    )
}

fn write_history_command(
    store: &dyn OperationalStore<String, String, String, String>,
    command: Command<String>,
    command_result: Option<uob_contracts::CommandResult>,
) {
    block_on(store.write_atomic(AtomicStoreWrite {
        purpose: uob_application::StorageWritePurpose::Routine,
        station_snapshot: None,
        authorization_changes: Vec::new(),
        command: Some(command),
        command_result,
        journal_events: Vec::new(),
        required_deliveries: Vec::new(),
        committed_records: Vec::new(),
    }))
    .expect("durable command");
}

fn child(id: &str) -> ResourceRef {
    ResourceRef {
        resource: Some(CanonicalResource::Connector {
            connector_id: text(CanonicalConnectorId::new, id),
        }),
        ..resource()
    }
}

fn write_command(
    store: &dyn OperationalStore<String, String, String, String>,
    id: &str,
    resource: ResourceRef,
    minute: u8,
    result: bool,
) {
    let mut command = command();
    command.request_id = text(RequestId::new, id);
    command.resource = resource;
    command.admitted_at = timestamp(minute);
    command.operation = CommandOperation::Start {
        authorization_reference: Some("private-authorization-reference".to_owned()),
    };
    let command_result = result.then(|| CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: None,
        resource: command.resource.clone(),
        return_route: CommandReturnRoute {
            request_id: command.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
        recorded_at: timestamp(6),
        observed_effects: vec![ObservedCommandEffect {
            event_id: text(EventId::new, "effect-1"),
            event_type: text(EventType::new, "charging.started.v1"),
            observed_at: timestamp(7),
        }],
        configuration: None,
        configuration_observations: Vec::new(),
    });
    write_history_command(store, command, command_result);
}

#[test]
fn command_history_applies_station_and_child_grants_before_newest_first_pagination() {
    let memory = MemoryStore::default();
    let replacement = ReplacementMemoryStore::default();
    for store in [
        &memory as &dyn OperationalStore<String, String, String, String>,
        &replacement as &dyn OperationalStore<String, String, String, String>,
    ] {
        write_command(store, "old-station", resource(), 0, false);
        write_command(store, "old-child", child("a"), 1, false);
        write_command(store, "excluded-child", child("b"), 5, false);
        let mut other_station = resource();
        other_station.station_id = text(StationId::new, "station-2");
        write_command(store, "excluded-station", other_station, 5, false);
        write_command(store, "y-station", resource(), 4, false);
        write_command(store, "z-child", child("a"), 4, true);

        let scope = CommandHistoryScope {
            descendants: false,
            station_only: true,
            resources: vec![child("a").resource.expect("connector")],
        };
        let mut query = CommandHistoryQuery {
            station: resource(),
            after: None,
            limit: PageLimit::new(1).expect("page limit"),
        };
        let empty_scope = CommandHistoryScope {
            descendants: false,
            station_only: false,
            resources: Vec::new(),
        };
        let empty = block_on(store.read_command_history(query.clone(), empty_scope))
            .expect("no grant returns no history");
        assert!(empty.items.is_empty());

        let all_children = CommandHistoryScope {
            descendants: true,
            station_only: false,
            resources: Vec::new(),
        };
        let newest = block_on(store.read_command_history(query.clone(), all_children))
            .expect("descendant history");
        assert_eq!(newest.items[0].request_id.as_str(), "excluded-child");

        let mut ids = Vec::new();
        let first = block_on(store.read_command_history(query.clone(), scope.clone()))
            .expect("scoped first page");
        assert_eq!(first.items[0].operation, CommandOperationKind::Start);
        assert_eq!(first.items[0].resource, child("a"));
        assert_eq!(first.items[0].recorded_at, Some(timestamp(6)));
        assert_eq!(first.items[0].observed_effects[0].observed_at, timestamp(7));
        assert!(matches!(
            first.items[0].lifecycle.as_ref(),
            Some(CommandLifecycle::ProtocolResponse { accepted: true, .. })
        ));
        let serialized = serde_json::to_string(&first.items[0]).expect("serialized summary");
        assert!(!serialized.contains("private-authorization-reference"));

        ids.push(first.items[0].request_id.as_str().to_owned());
        let first_cursor = first.next_cursor.expect("next page");

        let mut different_scope = scope.clone();
        different_scope.station_only = false;
        query.after = Some(first_cursor.clone());
        let error = block_on(store.read_command_history(query.clone(), different_scope))
            .expect_err("cursor from another grant scope");
        assert_eq!(error.code(), StorageErrorCode::InvalidRequest);
        let mut different_station = query.clone();
        different_station.station.station_id = text(StationId::new, "station-2");
        let error = block_on(store.read_command_history(different_station, scope.clone()))
            .expect_err("cursor from another station");
        assert_eq!(error.code(), StorageErrorCode::InvalidRequest);
        query.after = Some(text(CommandHistoryCursor::new, "uob:command:unknown"));
        let error = block_on(store.read_command_history(query.clone(), scope.clone()))
            .expect_err("unrecognized cursor");
        assert_eq!(error.code(), StorageErrorCode::InvalidRequest);

        query.after = Some(first_cursor.clone());
        let mut cursor = Some(first_cursor);
        while let Some(after) = cursor {
            query.after = Some(after);
            let page = block_on(store.read_command_history(query.clone(), scope.clone()))
                .expect("next scoped page");
            assert!(page.items.iter().all(|entry| entry.recorded_at.is_none()));
            ids.extend(
                page.items
                    .iter()
                    .map(|entry| entry.request_id.as_str().to_owned()),
            );
            cursor = page.next_cursor;
        }
        assert_eq!(ids, ["z-child", "y-station", "old-child", "old-station"]);

        query.station.resource = child("a").resource;
        let error = block_on(store.read_command_history(query, scope))
            .expect_err("history requires canonical station");
        assert_eq!(error.code(), StorageErrorCode::InvalidRequest);
    }
}

#[test]
fn command_history_cursor_expires_when_its_anchor_is_no_longer_retained() {
    let store = MemoryStore::default();
    for (id, minute) in [("older", 0), ("newer", 1)] {
        let mut admitted = command();
        admitted.request_id = text(RequestId::new, id);
        admitted.admitted_at = timestamp(minute);
        write_history_command(&store, admitted, None);
    }
    let scope = CommandHistoryScope {
        descendants: false,
        station_only: true,
        resources: Vec::new(),
    };
    let mut query = CommandHistoryQuery {
        station: resource(),
        after: None,
        limit: PageLimit::new(1).expect("page limit"),
    };
    let first =
        block_on(store.read_command_history(query.clone(), scope.clone())).expect("first page");
    assert_eq!(first.items[0].request_id.as_str(), "newer");
    query.after = first.next_cursor;
    store
        .state
        .lock()
        .expect("memory state")
        .commands
        .remove("newer");
    let error = block_on(store.read_command_history(query, scope))
        .expect_err("removed anchor expires cursor");
    assert_eq!(error.code(), StorageErrorCode::CursorExpired);
}
