use std::{
    collections::BTreeMap,
    future::Future,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    AtomicStoreWrite, AtomicWriteOutcome, CommandAdmissionErrorCode, CommandAdmissionOutcome,
    CommandAdmissionPort, CommandClock, CommandCoordinator, CommandDispatchOutcome,
    CommittedRecord, CommittedRecordCursor, CommittedRecordQuery, OperationalStore, Page,
    PageLimit, RecoveryBatch, RecoveryQuery, RetainedEventPage, RetainedEventQuery, SnapshotCursor,
    SnapshotQuery, StationCommandContext, StationCommandFuture, StationCommandPort,
    StorageAdmissionState, StorageFuture, StorageRetentionStatus,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, Command, CommandError, CommandErrorCode,
    CommandLifecycle, CommandOperation, CommandRequest, CommandResult, Connectivity,
    ContractVersion, EventId, EventType, ExternalCommand, ObservedCommandEffect, Operation,
    PrincipalId, RequestId, ResourceCapabilities, ResourceRef, StationId, StationSnapshot,
    SupportedOperation, UtcTimestamp,
};

type Coordinator = CommandCoordinator<String, String, String, String>;
#[path = "command_recovery/store.rs"]
mod store;

#[derive(Default)]
struct State {
    commands: BTreeMap<String, Command<String>>,
    results: BTreeMap<String, CommandResult>,
    reject_new_starts: bool,
}

#[derive(Default)]
struct MemoryStore(Mutex<State>);

struct FixedClock(UtcTimestamp);

impl CommandClock for FixedClock {
    fn now(&self) -> UtcTimestamp {
        self.0
    }
}

struct Stations {
    context: Mutex<Option<StationCommandContext>>,
    outcome: Mutex<CommandDispatchOutcome>,
    dispatches: Mutex<usize>,
}

impl Stations {
    fn new(connectivity: Connectivity, outcome: CommandDispatchOutcome) -> Self {
        Self {
            context: Mutex::new(Some(StationCommandContext {
                connectivity,
                capabilities: ResourceCapabilities {
                    operations: vec![SupportedOperation {
                        operation: Operation::Start,
                        parameters: Vec::new(),
                    }],
                    ..ResourceCapabilities::default()
                },
            })),
            outcome: Mutex::new(outcome),
            dispatches: Mutex::new(0),
        }
    }
}

impl StationCommandPort<String> for Stations {
    fn context(
        &self,
        _resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async move { Ok(self.context.lock().expect("station context").clone()) })
    }

    fn dispatch(
        &self,
        _command: Command<String>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            *self.dispatches.lock().expect("dispatch count") += 1;
            Ok(self.outcome.lock().expect("dispatch outcome").clone())
        })
    }
}

#[test]
fn offline_command_is_rejected_without_persistence_or_later_dispatch() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let stations = Arc::new(Stations::new(
            Connectivity::Disconnected,
            accepted_outcome(),
        ));
        let command_coordinator = coordinator(Arc::clone(&store), Arc::clone(&stations));

        let result = command_coordinator
            .submit(external())
            .await
            .expect("offline result");
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::Rejected {
                error: CommandError {
                    code: CommandErrorCode::StationDisconnected,
                    ..
                }
            }
        ));
        assert!(store.0.lock().expect("store state").commands.is_empty());

        stations
            .context
            .lock()
            .expect("station context")
            .as_mut()
            .expect("context")
            .connectivity = connected();
        assert_eq!(*stations.dispatches.lock().expect("dispatch count"), 0);
    });
}

#[test]
fn storage_pressure_rejects_every_start_ingress_before_dispatch() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        store.0.lock().expect("store state").reject_new_starts = true;
        let stations = Arc::new(Stations::new(connected(), accepted_outcome()));
        let command_coordinator = coordinator(Arc::clone(&store), Arc::clone(&stations));

        let error = command_coordinator
            .submit(external())
            .await
            .expect_err("storage pressure rejects start");
        assert_eq!(
            error.code(),
            CommandAdmissionErrorCode::StorageCapacityExhausted
        );
        assert_eq!(*stations.dispatches.lock().expect("dispatch count"), 0);
        assert!(store.0.lock().expect("store state").commands.is_empty());
    });
}

#[test]
fn uncertain_transmission_is_not_resent_and_observation_does_not_manufacture_success() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let stations = Arc::new(Stations::new(
            connected(),
            CommandDispatchOutcome::TransmissionUncertain {
                detail: "connection closed after transmission".to_owned(),
            },
        ));
        let command_coordinator = coordinator(Arc::clone(&store), Arc::clone(&stations));

        let result = command_coordinator
            .submit(external())
            .await
            .expect("uncertain result");
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
        assert_eq!(*stations.dispatches.lock().expect("dispatch count"), 1);

        let restarted = coordinator(Arc::clone(&store), Arc::clone(&stations));
        let recovery = restarted
            .recover_unresolved(PageLimit::new(10).expect("limit"))
            .await
            .expect("recovery");
        assert_eq!(recovery.commands.len(), 1);
        assert!(matches!(
            recovery.commands[0].result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
        assert_eq!(*stations.dispatches.lock().expect("dispatch count"), 1);

        let reconciled = restarted
            .reconcile_observed_effect(
                id(RequestId::new, "request-1"),
                ObservedCommandEffect {
                    event_id: id(EventId::new, "event-observed"),
                    event_type: id(EventType::new, "transaction.started"),
                    observed_at: timestamp(2),
                },
            )
            .await
            .expect("reconcile")
            .expect("known command");
        assert!(matches!(
            reconciled.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
        assert_eq!(reconciled.observed_effects.len(), 1);

        let duplicate = restarted
            .submit(external())
            .await
            .expect("duplicate result");
        assert_eq!(duplicate, reconciled);
        assert_eq!(*stations.dispatches.lock().expect("dispatch count"), 1);
    });
}

#[test]
fn restart_converts_in_flight_dispatch_to_uncertain_without_replay() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let stations = Arc::new(Stations::new(connected(), accepted_outcome()));
        let command = external().admit(timestamp(0));
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(command.clone());
        write.command_result = Some(result(&command, CommandLifecycle::Dispatched));
        store
            .write_atomic(write)
            .await
            .expect("seed in-flight command");

        let recovered = coordinator(Arc::clone(&store), Arc::clone(&stations))
            .recover_unresolved(PageLimit::new(10).expect("limit"))
            .await
            .expect("recover in-flight command");
        assert!(matches!(
            recovered.commands[0].result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ));
        assert_eq!(*stations.dispatches.lock().expect("dispatch count"), 0);
    });
}

fn coordinator(store: Arc<MemoryStore>, stations: Arc<Stations>) -> Coordinator {
    CommandCoordinator::new(store, stations, Arc::new(FixedClock(timestamp(1))))
}

fn external() -> ExternalCommand<String> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: id(RequestId::new, "request-1"),
            correlation_id: None,
            resource: resource(),
            operation: CommandOperation::Start {
                authorization_reference: None,
            },
            expires_at: timestamp(10),
        },
        AuthenticatedCommandOrigin::Management {
            principal_id: id(PrincipalId::new, "operator-1"),
        },
    )
}

fn result(command: &Command<String>, lifecycle: CommandLifecycle) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: None,
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at: timestamp(1),
        observed_effects: Vec::new(),
    }
}

fn accepted_outcome() -> CommandDispatchOutcome {
    CommandDispatchOutcome::ProtocolResponse {
        accepted: true,
        error: None,
    }
}

fn connected() -> Connectivity {
    Connectivity::Connected {
        protocol: uob_contracts::ProtocolEdition::Ocpp16j,
        connected_at: timestamp(0),
        last_message_at: Some(timestamp(1)),
    }
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: id(BridgeId::new, "bridge-1"),
        station_id: id(StationId::new, "station-1"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp(hour: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 2).expect("date"),
            Time::from_hms(hour, 0, 0).expect("time"),
        )
        .assume_offset(UtcOffset::UTC),
    )
}

fn id<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid identity")
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
