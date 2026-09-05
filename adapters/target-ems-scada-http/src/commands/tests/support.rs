use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use uob_application::{
    CanonicalQuerySource, CommandAdmissionPort, CommandClock, CommandCoordinator,
    CommandDispatchOutcome, OperationalStore, RetainedEventQuery, ScopedTargetQueryPort,
    StationCommandContext, StationCommandFuture, StationCommandPort, TargetPortFuture, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryResult, TargetResourceScope,
    TargetRetainedEventStream,
};
use uob_contracts::{
    BridgeId, Command, Connectivity, Operation, ResourceCapabilities, ResourceRef,
    SupportedOperation, TargetInstanceId, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;

use crate::{
    commands::{CommandExecutor, SupervisedCommands},
    reads::{ReadExecutor, SupervisedReads},
    routing::{IntegrationState, integration_router},
    test_support::{credentials_from, descriptor, listener_limits},
};

pub(super) type Store = SqliteOperationalStore<Value, Value, Value, Value>;
pub(super) type Coordinator = CommandCoordinator<Value, Value, Value, Value>;

pub(super) struct Stations(pub AtomicUsize);

impl StationCommandPort<Value> for Stations {
    fn context(
        &self,
        resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async move {
            Ok(Some(StationCommandContext {
                connectivity: if resource.station_id.as_str() == "offline" {
                    Connectivity::Disconnected
                } else {
                    Connectivity::Connected {
                        protocol: uob_contracts::ProtocolEdition::Ocpp16j,
                        connected_at: Clock.now(),
                        last_message_at: None,
                    }
                },
                capabilities: ResourceCapabilities {
                    operations: vec![SupportedOperation {
                        operation: Operation::Start,
                        parameters: vec![],
                    }],
                    ..ResourceCapabilities::default()
                },
            }))
        })
    }

    fn dispatch(&self, _: Command<Value>) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(CommandDispatchOutcome::ProtocolResponse {
                accepted: true,
                error: None,
            })
        })
    }
}

pub(super) struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::new(time::OffsetDateTime::now_utc())
    }
}

pub(super) struct Source(pub Arc<Store>);
impl CanonicalQuerySource<Value> for Source {
    fn query<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<Value>> {
        Box::pin(async move {
            let TargetQuery::CommandResult(id) = query else {
                panic!("unexpected query")
            };
            Ok(TargetQueryResult::CommandResult(
                self.0.command_result_by_request_id(id).await.unwrap(),
            ))
        })
    }
    fn subscribe_retained_events<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        _: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<Value>> {
        Box::pin(async { panic!("unexpected subscription") })
    }
}

pub(super) struct Harness {
    pub store: Arc<Store>,
    pub coordinator: Arc<Coordinator>,
    pub stations: Arc<Stations>,
    pub path: PathBuf,
}

impl Harness {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "uob-http-command-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let store = Arc::new(Store::open(path.join("store.sqlite"), 16).unwrap());
        let stations = Arc::new(Stations(AtomicUsize::new(0)));
        let coordinator = Arc::new(Coordinator::new(
            store.clone(),
            stations.clone(),
            Arc::new(Clock),
        ));
        Self {
            store,
            coordinator,
            stations,
            path,
        }
    }

    pub fn router(&self) -> Router {
        self.router_with(self.coordinator.clone(), Duration::from_secs(2))
    }

    pub fn router_with(
        &self,
        admission: Arc<dyn CommandAdmissionPort<Value>>,
        deadline: Duration,
    ) -> Router {
        router(self.store.clone(), admission, deadline)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(super) fn router(
    store: Arc<Store>,
    admission: Arc<dyn CommandAdmissionPort<Value>>,
    deadline: Duration,
) -> Router {
    let credentials = credentials_from(concat!(
        "[[principals]]\nid = 'reader'\ntoken = 'reader'\npermissions = ['read']\nbridges = ['site-01']\n",
        "[[principals]]\nid = 'operator'\ntoken = 'operator'\npermissions = ['control']\nbridges = ['site-01']\n",
        "[[principals]]\nid = 'station-operator'\ntoken = 'station-operator'\npermissions = ['control']\n",
        "[[principals.stations]]\nbridge_id = 'site-01'\nstation_id = 'station-a'\n",
    ));
    let queries = ScopedTargetQueryPort::new(
        Arc::new(Source(store)),
        TargetQueryAuthorization::new(
            TargetInstanceId::new("main").unwrap(),
            vec![TargetQueryPermission::CommandStatus],
            ["station-a", "station-b", "offline"]
                .into_iter()
                .map(|station| TargetResourceScope::Station {
                    bridge_id: BridgeId::new("site-01").unwrap(),
                    station_id: uob_contracts::StationId::new(station).unwrap(),
                })
                .collect(),
        ),
    );
    integration_router(
        IntegrationState::new(
            descriptor(),
            listener_limits(16),
            credentials,
            ReadExecutor::new(Arc::new(SupervisedReads::new(Arc::new(queries))), deadline),
        )
        .with_commands(CommandExecutor::new(
            Arc::new(SupervisedCommands::new(admission)),
            deadline,
            1,
        )),
    )
}

pub(super) fn payload(id: &str, station: &str) -> Value {
    json!({
        "request_id": id,
        "resource": {"bridge_id": "site-01", "station_id": station},
        "operation": {"kind": "start", "parameters": {}},
        "expires_at": "2099-01-01T00:00:00Z"
    })
}

pub(super) async fn post(router: Router, token: &str, payload: Value) -> (StatusCode, Value) {
    send(
        router,
        "POST",
        "/bridge/v1/commands",
        token,
        Body::from(payload.to_string()),
    )
    .await
}

pub(super) async fn send(
    router: Router,
    method: &str,
    path: &str,
    token: &str,
    body: Body,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let value = serde_json::from_slice(&body).unwrap();
    crate::openapi::tests::assert_response(method, path, status.as_u16(), &value);
    (status, value)
}
