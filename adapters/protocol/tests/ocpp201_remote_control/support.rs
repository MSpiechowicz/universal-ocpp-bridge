use crate::endpoint_support::{self, SECRET, TEST_BOUND};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use uob_application::charging_identity::{
    ChargingIdentityProvider, ChargingIdentityResolution, ChargingTokenKind,
    PresentedChargingIdentity,
};
use uob_application::*;
use uob_contracts::*;
use uob_hostile_websocket_peer::{Peer, PeerConfig};
use uob_protocol_adapter::{
    CallSessionConfiguration, CallSessionHandle, CallSessionOutputs, CallSessionTask, OcppEndpoint,
    spawn_call_session,
    v201::{self, remote_control::*},
};
use uob_provider_adapter::LocalChargingIdentityProvider;
use uob_storage_adapter::SqliteOperationalStore;
pub struct RunningSession {
    pub peer: Peer,
    pub handle: CallSessionHandle,
    pub outputs: CallSessionOutputs,
    pub task: CallSessionTask,
    pub server: JoinHandle<()>,
}

pub async fn session(protocol: &str, response_timeout: Duration) -> RunningSession {
    session_with_diagnostics(
        protocol,
        response_timeout,
        uob_application::FlowDiagnostics::default(),
    )
    .await
}

async fn session_with_diagnostics(
    protocol: &str,
    response_timeout: Duration,
    diagnostics: uob_application::FlowDiagnostics,
) -> RunningSession {
    let application =
        endpoint_support::application(Environment::Demo, None).with_diagnostics(diagnostics);
    let (endpoint, mut accepted) = OcppEndpoint::new(
        endpoint_support::authenticator(
            uob_protocol_adapter::StationAuthenticationMode::Credential,
            None,
        ),
        &application,
        4,
    )
    .expect("endpoint");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        endpoint
            .serve_plaintext(listener)
            .await
            .expect("endpoint server");
    });
    let authorization = STANDARD.encode([b"alpha:".as_slice(), SECRET].concat());
    let peer = Peer::connect(PeerConfig {
        endpoint: format!("ws://{address}/ocpp/alpha"),
        subprotocol: protocol.to_owned(),
        max_outbound_bytes: 512 * 1024,
        max_inbound_bytes: 512 * 1024,
        observation_capacity: 64,
        authorization: Some(format!("Basic {authorization}")),
    })
    .await
    .expect("raw authenticated peer");
    let connection = timeout(TEST_BOUND, accepted.receive())
        .await
        .expect("endpoint handoff bound")
        .expect("endpoint handoff");
    let (handle, outputs, task) = spawn_call_session(
        connection,
        &application,
        CallSessionConfiguration {
            pending_call_capacity: 8,
            incoming_call_capacity: 8,
            diagnostic_capacity: 16,
            response_timeout,
        },
    )
    .expect("call session");
    RunningSession {
        peer,
        handle,
        outputs,
        task,
        server,
    }
}

pub async fn receive_json(peer: &mut Peer) -> Value {
    let message = timeout(TEST_BOUND, peer.receive())
        .await
        .expect("peer receive bound")
        .expect("peer frame");
    let text = message.into_text().expect("text frame");
    serde_json::from_str(&text).expect("JSON frame")
}

pub type Store = SqliteOperationalStore<Value, TransactionSnapshot, TransactionSnapshot, String>;
pub type Coordinator = CommandCoordinator<Value, TransactionSnapshot, TransactionSnapshot, String>;
pub type Auth = LocalAuthorizationService<Value, TransactionSnapshot, TransactionSnapshot, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-remote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn open(&self) -> Store {
        Store::open(self.0.join("state.db"), 32).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn time(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}
pub struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        time("2026-09-01T02:00:00Z")
    }
}
pub fn origin() -> AuthenticatedCommandOrigin {
    AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("operator").unwrap(),
    }
}
pub fn scoped(
    coordinator: Arc<Coordinator>,
    snapshot: &StationSnapshot,
    permissions: Vec<AccessPermission>,
) -> ScopedCommandAdmissionPort<Value> {
    ScopedCommandAdmissionPort::new(
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
    )
}
pub fn protocol(action: &str, payload: Value) -> CommandOperation<Value> {
    CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(format!("urn:OCPP:Cp:2:2020:3:{action}Request"))
            .unwrap(),
        payload,
    })
}
pub fn command(
    snapshot: &StationSnapshot,
    id: &str,
    operation: CommandOperation<Value>,
) -> ExternalCommand<Value> {
    let resource = if matches!(&operation, CommandOperation::Ocpp(op) if op.action.as_str()=="Reset")
    {
        snapshot.station.clone()
    } else if matches!(&operation, CommandOperation::Ocpp(op) if op.action.as_str()=="UnlockConnector")
    {
        snapshot.resources[1].resource.clone()
    } else {
        snapshot.resources[0].resource.clone()
    };
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: RequestId::new(id).unwrap(),
            correlation_id: None,
            resource,
            operation,
            expires_at: time("2026-09-02T00:00:00Z"),
        },
        origin(),
    )
}
pub fn token() -> PresentedChargingIdentity {
    PresentedChargingIdentity {
        token: "INDEPENDENT-001".to_owned(),
        kind: ChargingTokenKind::Central,
        additional: vec![],
        certificate: None,
        certificate_hashes: vec![],
    }
}
pub async fn reference() -> AuthorizationReference {
    let ChargingIdentityResolution::Resolved { reference, .. } = LocalChargingIdentityProvider
        .resolve(&token())
        .await
        .unwrap()
    else {
        panic!("local reference")
    };
    reference
}
pub async fn setup(
    store: &Store,
    handle: CallSessionHandle,
) -> (
    StationSnapshot,
    Arc<Auth>,
    Arc<RemoteControlSession>,
    Arc<Coordinator>,
) {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    snapshot.station.station_id = StationId::new("alpha").unwrap();
    for entry in &mut snapshot.resources {
        entry.resource.station_id = snapshot.station.station_id.clone();
    }
    let mut evse = snapshot.resources[0].clone();
    let Some(CanonicalResource::Evse { connector_id, .. }) = &mut evse.resource.resource else {
        panic!("EVSE")
    };
    *connector_id = None;
    evse.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    snapshot.resources.insert(0, evse);
    for entry in &mut snapshot.resources {
        entry.capabilities.operations.push(SupportedOperation {
            operation: Operation::Stop,
            parameters: vec![],
        });
    }
    snapshot.transactions.clear();
    snapshot.observed_at = Clock.now();
    snapshot.resources[0].availability = AvailabilityState::Available;
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: "Reset".to_owned(),
        },
        parameters: vec![],
    });
    snapshot.resources[1]
        .capabilities
        .operations
        .push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: "UnlockConnector".to_owned(),
            },
            parameters: vec![],
        });
    v201::registration_call(
        include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/boot-notification.json"),
        store,
        &mut snapshot,
        registration::RegistrationDecision::Accepted,
        60,
        Clock.now(),
    )
    .await
    .unwrap();
    let auth = Arc::new(
        Auth::recover(Arc::new(store.clone()), PageLimit::new(100).unwrap())
            .await
            .unwrap(),
    );
    auth.apply_change(AuthorizationChange {
        reference: reference().await,
        resource: snapshot.resources[0].resource.clone(),
        state: AuthorizationState::Active,
        revision: 1,
        changed_at: Clock.now(),
        expires_at: None,
    })
    .await
    .unwrap();
    let identity = Arc::new(
        LocalRemoteStartIdentity::new(vec![token()], &LocalChargingIdentityProvider, auth.clone())
            .await
            .unwrap(),
    );
    let port = Arc::new(
        RemoteControlSession::new(
            handle,
            snapshot.clone(),
            identity,
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
    (snapshot, auth, port, coordinator)
}
pub fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
pub fn accepted(result: &CommandResult) {
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None
        }
    ));
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
