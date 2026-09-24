use super::{STATION_SECRET, proxy, query::Store};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{RwLock, watch},
    task::JoinHandle,
};
use uob_application::charging_identity::{
    ChargingIdentityProvider, ChargingIdentityResolution, ChargingTokenKind,
    PresentedChargingIdentity,
};
use uob_application::{
    Application, AuthorizationChange, AuthorizationProvider, AuthorizationState,
    ChargerObservation, CommandClock, CommandCoordinator, CommandDispatchOutcome,
    LocalAuthorizationService, PageLimit, SensitiveAuthorizationToken, StationCommandContext,
    StationCommandError, StationCommandFuture, StationCommandPort, StationEvent,
    TransactionEventKind, registration::RegistrationDecision,
};
use uob_contracts::{
    ArtifactDigest, AvailabilityState, BridgeId, CanonicalResource, CommandOperation, Environment,
    NativeProtocolReference, Operation, ProcessInstanceId, ProtocolEdition, ReleaseId, ResourceRef,
    RuntimeIdentity, ServiceIdentity, StationId, StationSnapshot, SupportedOperation,
    TargetInstanceId, TransactionSnapshot, UtcTimestamp,
};
use uob_protocol_adapter::{
    CallSessionConfiguration, CallSessionHandle, CallSessionOutputs, CallSessionTask, OcppEndpoint,
    ResolvedStationCredential, StationAuthenticationMode, StationAuthenticator,
    StationConnectionReceiver, StationCredential, StationRegistration,
    StationSecurityConfiguration, StationTlsReferences, spawn_call_session, v16, v201,
};
use uob_provider_adapter::{LocalAuthorizationProvider, LocalChargingIdentityProvider};

#[path = "protocol_transactions.rs"]
mod transactions;
pub struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        time("2026-09-01T02:00:00Z")
    }
}
fn time(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}
fn identity() -> ServiceIdentity {
    ServiceIdentity {
        bridge_id: BridgeId::new("site-01").unwrap(),
        runtime: RuntimeIdentity {
            environment: Environment::Demo,
            release_id: ReleaseId::new("test-issue87").unwrap(),
            release_digest: ArtifactDigest::new("sha256:issue87").unwrap(),
            process_instance_id: ProcessInstanceId::new("test-issue87").unwrap(),
        },
        selected_target_id: Some(TargetInstanceId::new("main").unwrap()),
    }
}
fn fixture(version: ProtocolEdition, station: &str) -> StationSnapshot {
    let bytes = if version == ProtocolEdition::Ocpp16j {
        include_bytes!("../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json")
            .as_slice()
    } else {
        include_bytes!("../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json")
            .as_slice()
    };
    let mut snapshot: StationSnapshot = serde_json::from_slice(bytes).unwrap();
    snapshot.station.bridge_id = BridgeId::new("site-01").unwrap();
    snapshot.station.station_id = StationId::new(station).unwrap();
    for item in &mut snapshot.resources {
        item.resource.bridge_id = snapshot.station.bridge_id.clone();
        item.resource.station_id = snapshot.station.station_id.clone();
    }
    snapshot.transactions.clear();
    snapshot.observed_at = Clock.now();
    snapshot.resources[0].availability = AvailabilityState::Available;
    if version == ProtocolEdition::Ocpp201 {
        snapshot.resources[0].current_values = serde_json::from_value(json!([{
            "point_id":"station-201.evse-1.power.active",
            "value":{"type":"decimal","value":"7.2"},
            "observed_at":Clock.now(),
            "quality":{"level":"good"},
            "freshness":{"status":"fresh","valid_until":"2026-09-02T00:00:00Z"}
        }]))
        .unwrap();
    }
    if version == ProtocolEdition::Ocpp201 {
        let mut evse = snapshot.resources[0].clone();
        if let Some(CanonicalResource::Evse { connector_id, .. }) = &mut evse.resource.resource {
            *connector_id = None;
        }
        evse.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
            evse_id: 1,
            connector_id: None,
        });
        snapshot.resources.insert(0, evse);
    }
    for item in &mut snapshot.resources {
        if !item
            .capabilities
            .operations
            .iter()
            .any(|op| op.operation == Operation::Stop)
        {
            item.capabilities.operations.push(SupportedOperation {
                operation: Operation::Stop,
                parameters: vec![],
            });
        }
        for value in &item.current_values {
            item.data_points.push(
                serde_json::from_value(json!({
                    "point_id":value.point_id, "resource":item.resource,
                    "semantic_name":"power.active.import", "value_type":"decimal",
                    "unit":"watt", "access":"read_only"
                }))
                .unwrap(),
            );
        }
    }
    snapshot
}
fn authenticator() -> StationAuthenticator {
    let stations = ["station-a", "station-b"].map(|name| StationRegistration {
        station_id: StationId::new(name).unwrap(),
        credential: uob_application::CredentialReference::new(format!("test:{name}")).unwrap(),
        client_certificate: None,
    });
    let configuration = StationSecurityConfiguration {
        authentication: StationAuthenticationMode::Credential,
        tls: StationTlsReferences {
            server_certificate_chain: uob_application::CredentialReference::new("test:cert")
                .unwrap(),
            server_private_key: uob_application::CredentialReference::new("test:key").unwrap(),
            client_certificate_authorities: None,
        },
        stations: stations.into(),
    }
    .validate()
    .unwrap();
    let credentials = ["station-a", "station-b"]
        .into_iter()
        .map(|name| ResolvedStationCredential {
            station_id: StationId::new(name).unwrap(),
            credential: StationCredential::from_secret(
                format!("{STATION_SECRET}-{name}").as_bytes(),
            )
            .unwrap(),
        })
        .collect();
    StationAuthenticator::new(configuration, credentials).unwrap()
}

type Auth = LocalAuthorizationService<Value, StationEvent, TransactionSnapshot, String>;
type Port = dyn StationCommandPort<Value>;
type Coordinator = CommandCoordinator<Value, StationEvent, TransactionSnapshot, String>;
enum Control {
    V16(Arc<v16::remote_control::RemoteControlSession>),
    V201(Arc<v201::remote_control::RemoteControlSession>),
}
#[derive(Clone)]
struct StationContext {
    store: Store,
    auth: Arc<Auth>,
    sessions: Arc<Sessions>,
}
#[derive(Clone)]
struct StationSession {
    port: Arc<Port>,
    stop_ready: watch::Receiver<bool>,
}
struct Sessions(RwLock<BTreeMap<String, StationSession>>);
impl StationCommandPort<Value> for Sessions {
    fn context(
        &self,
        resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async move {
            let session = self
                .0
                .read()
                .await
                .get(resource.station_id.as_str())
                .cloned();
            if let Some(session) = session {
                session.port.context(resource).await
            } else {
                Ok(None)
            }
        })
    }
    fn dispatch(
        &self,
        command: uob_contracts::Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            let session = self
                .0
                .read()
                .await
                .get(command.resource.station_id.as_str())
                .cloned();
            let mut session =
                session.ok_or_else(|| StationCommandError::new("station disconnected"))?;
            if matches!(&command.operation, CommandOperation::Stop { .. }) {
                // The store commits the start before the charger receives its response.
                // Its next Heartbeat is sent only after the simulator records that response.
                while !*session.stop_ready.borrow_and_update() {
                    session
                        .stop_ready
                        .changed()
                        .await
                        .map_err(|_| StationCommandError::new("station disconnected"))?;
                }
            }
            session.port.dispatch(command).await
        })
    }
}

async fn authorization(store: &Store) -> (Arc<Auth>, StationSnapshot, StationSnapshot) {
    let authorization = Arc::new(
        Auth::recover(Arc::new(store.clone()), PageLimit::new(100).unwrap())
            .await
            .unwrap(),
    );
    let one = fixture(ProtocolEdition::Ocpp16j, "station-a");
    let two = fixture(ProtocolEdition::Ocpp201, "station-b");
    for (snapshot, reference) in [
        (
            &one,
            LocalAuthorizationProvider
                .resolve(&SensitiveAuthorizationToken::new("LOCAL-USER-1").unwrap())
                .await
                .unwrap(),
        ),
        (
            &two,
            match LocalChargingIdentityProvider
                .resolve(&token())
                .await
                .unwrap()
            {
                ChargingIdentityResolution::Resolved { reference, .. } => reference,
                ChargingIdentityResolution::Denied { .. } => panic!("test identity not resolved"),
            },
        ),
    ] {
        authorization
            .apply_change(AuthorizationChange {
                reference,
                resource: snapshot.resources[0].resource.clone(),
                state: AuthorizationState::Active,
                revision: 1,
                changed_at: Clock.now(),
                expires_at: None,
            })
            .await
            .unwrap();
    }
    (authorization, one, two)
}

async fn control(
    version: ProtocolEdition,
    handle: CallSessionHandle,
    snapshot: &StationSnapshot,
    context: &StationContext,
) -> Control {
    if version == ProtocolEdition::Ocpp16j {
        Control::V16(Arc::new(
            v16::remote_control::RemoteControlSession::new(
                handle,
                snapshot.clone(),
                Arc::new(
                    v16::remote_control::LocalRemoteStartIdentity::new(
                        vec![SensitiveAuthorizationToken::new("LOCAL-USER-1").unwrap()],
                        context.auth.clone(),
                    )
                    .unwrap(),
                ),
                Arc::new(Clock),
                Arc::new(context.store.clone()),
            )
            .unwrap(),
        ))
    } else {
        Control::V201(Arc::new(
            v201::remote_control::RemoteControlSession::new(
                handle,
                snapshot.clone(),
                Arc::new(
                    v201::remote_control::LocalRemoteStartIdentity::new(
                        vec![token()],
                        &LocalChargingIdentityProvider,
                        context.auth.clone(),
                    )
                    .await
                    .unwrap(),
                ),
                Arc::new(Clock),
                Arc::new(context.store.clone()),
            )
            .unwrap(),
        ))
    }
}

async fn run_station(
    station: String,
    version: ProtocolEdition,
    handle: CallSessionHandle,
    mut outputs: CallSessionOutputs,
    task: CallSessionTask,
    context: StationContext,
) {
    let mut snapshot = fixture(version, &station);
    let control = control(version, handle, &snapshot, &context).await;
    let command_port: Arc<Port> = match &control {
        Control::V16(port) => port.clone(),
        Control::V201(port) => port.clone(),
    };
    let (stop_ready, receiver) = watch::channel(false);
    context.sessions.0.write().await.insert(
        station.clone(),
        StationSession {
            port: command_port,
            stop_ready: receiver,
        },
    );
    let mut sequence = 0;
    while let Some(incoming) = outputs.incoming.receive().await {
        sequence += 1;
        let action = incoming.call.action.as_str().to_owned();
        let started = action == "StartTransaction"
            || matches!(
                &incoming.call.observation,
                ChargerObservation::TransactionEvent(observation)
                    if observation.event == TransactionEventKind::Started
            );
        if started {
            stop_ready.send_replace(false);
        }
        if matches!(
            action.as_str(),
            "BootNotification" | "StatusNotification" | "Heartbeat"
        ) {
            incoming
                .complete_registration(
                    &context.store,
                    &mut snapshot,
                    RegistrationDecision::Accepted,
                    60,
                    Clock.now(),
                )
                .await
                .unwrap();
        } else if version == ProtocolEdition::Ocpp16j {
            transactions::handle_16(
                incoming,
                &context.store,
                &context.auth,
                &mut snapshot,
                sequence,
            )
            .await;
        } else {
            transactions::handle_201(
                incoming,
                &context.store,
                &context.auth,
                &mut snapshot,
                sequence,
            )
            .await;
        }
        // The remote-control session observes only committed station state.
        match &control {
            Control::V16(port) => port.update_committed(snapshot.clone()).unwrap(),
            Control::V201(port) => port.update_committed(snapshot.clone()).unwrap(),
        }
        if action == "Heartbeat" {
            stop_ready.send_replace(true);
        }
    }
    context.sessions.0.write().await.remove(&station);
    let _ = task.shutdown(Duration::from_secs(2)).await;
}

fn station_loop(
    mut connections: StationConnectionReceiver,
    application: Application,
    context: StationContext,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(connection) = connections.receive().await {
            let station = connection.station().station_id.as_str().to_owned();
            let version = connection.station().protocol;
            let (handle, outputs, task) = spawn_call_session(
                connection,
                &application,
                CallSessionConfiguration {
                    pending_call_capacity: 8,
                    incoming_call_capacity: 8,
                    diagnostic_capacity: 16,
                    response_timeout: Duration::from_secs(5),
                },
            )
            .unwrap();
            tokio::spawn(run_station(
                station,
                version,
                handle,
                outputs,
                task,
                context.clone(),
            ));
        }
    })
}

pub struct ProtocolHost {
    pub coordinator: Arc<Coordinator>,
    pub proxy_address: String,
    pub proxy_201_address: String,
    endpoint: JoinHandle<()>,
    station_loop: JoinHandle<()>,
    proxy: JoinHandle<()>,
    proxy_201: JoinHandle<()>,
}
impl Drop for ProtocolHost {
    fn drop(&mut self) {
        self.endpoint.abort();
        self.station_loop.abort();
        self.proxy.abort();
        self.proxy_201.abort();
    }
}
impl ProtocolHost {
    pub async fn start(store: Store) -> Self {
        let application = Application::new(identity());
        let (authorization, one, two) = authorization(&store).await;
        let sessions = Arc::new(Sessions(RwLock::new(BTreeMap::new())));
        let coordinator = Arc::new(Coordinator::new(
            Arc::new(store.clone()),
            sessions.clone(),
            Arc::new(Clock),
        ));
        let (endpoint, connections) = OcppEndpoint::new(authenticator(), &application, 4).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = tokio::spawn(async move {
            endpoint.serve_plaintext(listener).await.unwrap();
        });
        let station_loop = station_loop(
            connections,
            application,
            StationContext {
                store,
                auth: authorization.clone(),
                sessions,
            },
        );
        let (proxy_address, proxy) = proxy::start(
            format!("ws://{address}/ocpp/station-a"),
            authorization.clone(),
            one.resources[0].resource.clone(),
            "ocpp1.6",
        )
        .await;
        let (proxy_201_address, proxy_201) = proxy::start(
            format!("ws://{address}/ocpp/station-b"),
            authorization,
            two.resources[0].resource.clone(),
            "ocpp2.0.1",
        )
        .await;
        Self {
            coordinator,
            proxy_address,
            proxy_201_address,
            endpoint,
            station_loop,
            proxy,
            proxy_201,
        }
    }
}
fn token() -> PresentedChargingIdentity {
    PresentedChargingIdentity {
        token: "LOCAL-USER-1".into(),
        kind: ChargingTokenKind::Central,
        additional: vec![],
        certificate: None,
        certificate_hashes: vec![],
    }
}
