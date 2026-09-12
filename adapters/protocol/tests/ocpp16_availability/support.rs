pub use crate::remote_support::{
    Clock, Database, RunningSession, fixture, origin, protocol, receive_json, session, time,
};
use serde_json::{Value, json};
use std::sync::Arc;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::{
    CallSessionHandle,
    v16::{self, remote_control::*},
};
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, StationSnapshot, StationSnapshot, String>;
pub type Coordinator = CommandCoordinator<Value, StationSnapshot, StationSnapshot, String>;
pub struct Identity;
impl RemoteStartIdentity for Identity {
    fn authorized_token(
        &self,
        _: &str,
        _: &ResourceRef,
        _: UtcTimestamp,
    ) -> Option<SensitiveAuthorizationToken> {
        None
    }
}
pub fn open(database: &Database) -> Store {
    Store::open(database.0.join("state.db"), 32).unwrap()
}
pub async fn setup(
    store: &Store,
    handle: CallSessionHandle,
) -> (StationSnapshot, Arc<RemoteControlSession>, Arc<Coordinator>) {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    snapshot.station.station_id = StationId::new("alpha").unwrap();
    snapshot.transactions.clear();
    snapshot.observed_at = Clock.now();
    let operation = SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "ChangeAvailability".to_owned(),
        },
        parameters: vec![],
    };
    snapshot.capabilities.operations.push(operation.clone());
    for entry in &mut snapshot.resources {
        entry.resource.station_id = snapshot.station.station_id.clone();
        entry.availability = AvailabilityState::Available;
        entry.current_values.clear();
        entry.capabilities.operations.push(operation.clone());
    }
    v16::registration_call(
        &serde_json::to_vec(&fixture("boot-notification")).unwrap(),
        store,
        &mut snapshot,
        registration::RegistrationDecision::Accepted,
        60,
        Clock.now(),
    )
    .await
    .unwrap();
    let port = Arc::new(
        RemoteControlSession::new(
            handle,
            snapshot.clone(),
            Arc::new(Identity),
            Arc::new(Clock),
            Arc::new(store.clone()),
        )
        .unwrap(),
    );
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port.clone(),
        Arc::new(Clock),
    ));
    (snapshot, port, coordinator)
}
pub fn command(
    snapshot: &StationSnapshot,
    id: &str,
    connector: u32,
    kind: &str,
) -> ExternalCommand<Value> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: RequestId::new(id).unwrap(),
            correlation_id: None,
            resource: if connector == 0 {
                snapshot.station.clone()
            } else {
                snapshot
                    .resources
                    .iter()
                    .find(|r| {
                        r.resource.native_protocol_reference
                            == Some(NativeProtocolReference::Ocpp16 {
                                connector_id: connector,
                            })
                    })
                    .unwrap()
                    .resource
                    .clone()
            },
            operation: protocol(
                "ChangeAvailability",
                json!({"connectorId":connector,"type":kind}),
            ),
            expires_at: time("2026-09-02T00:00:00Z"),
        },
        origin(),
    )
}
pub fn scoped(
    coordinator: Arc<Coordinator>,
    snapshot: &StationSnapshot,
    permissions: Vec<AccessPermission>,
) -> Arc<ScopedCommandAdmissionPort<Value>> {
    Arc::new(ScopedCommandAdmissionPort::new(
        coordinator,
        AccessPolicy::single(
            AccessGrant::new(
                origin(),
                permissions,
                vec![AccessResourceScope::Station {
                    bridge_id: snapshot.station.bridge_id.clone(),
                    station_id: snapshot.station.station_id.clone(),
                }],
            )
            .unwrap(),
        ),
    ))
}
pub async fn persisted(store: &Store) -> StationSnapshot {
    store
        .read_snapshots(SnapshotQuery {
            after: None,
            limit: PageLimit::new(1).unwrap(),
        })
        .await
        .unwrap()
        .items
        .remove(0)
}
pub async fn event(store: &Store, snapshot: &StationSnapshot) -> EventEnvelope<StationSnapshot> {
    store
        .read_retained_events(RetainedEventQuery {
            resource: snapshot.station.clone(),
            after: None,
            limit: PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .events
        .pop()
        .unwrap()
}
pub fn context(
    snapshot: &StationSnapshot,
    sequence: u64,
) -> registration::availability::AvailabilityContext {
    registration::availability::AvailabilityContext {
        identity: serde_json::from_value(json!({"bridge_id":snapshot.station.bridge_id,"runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test","process_instance_id":"test"}})).unwrap(),
        event_id: EventId::new(format!("availability-{sequence}")).unwrap(), sequence,
    }
}
pub async fn status(
    running: &mut RunningSession,
    store: &Store,
    snapshot: &mut StationSnapshot,
    connector: u32,
    state: &str,
    sequence: u64,
) {
    let mut frame = fixture("availability-status");
    frame[1] = json!(format!("status-{sequence}"));
    frame[3]["connectorId"] = json!(connector);
    frame[3]["status"] = json!(state);
    running.peer.send_text(frame.to_string()).await.unwrap();
    let incoming = running.outputs.incoming.receive().await.unwrap();
    let context = context(snapshot, sequence);
    let response =
        v16::availability::complete_status(incoming.call, store, snapshot, context, Clock.now())
            .await
            .unwrap();
    assert_eq!(persisted(store).await, *snapshot);
    incoming.responder.respond(&response[2]).unwrap();
    assert_eq!(receive_json(&mut running.peer).await, response);
}
pub async fn exchange(
    running: &mut RunningSession,
    commands: Arc<ScopedCommandAdmissionPort<Value>>,
    external: ExternalCommand<Value>,
    status: &str,
) -> CommandResult {
    let id = external.request.request_id.clone();
    let expected = match &external.request.operation {
        CommandOperation::Ocpp(op) => op.payload.clone(),
        _ => unreachable!(),
    };
    let submit = tokio::spawn(async move { commands.submit(external).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2, id, "ChangeAvailability", expected])
    );
    let mut response = fixture(&format!("availability-{}", status.to_lowercase()));
    response[1] = json!(id);
    running.peer.send_text(response.to_string()).await.unwrap();
    submit.await.unwrap()
}
