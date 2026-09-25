use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, Application,
    CanonicalQuerySource, CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture,
    CommandAdmissionPort, Page, ScopedCommandAdmissionPort, SnapshotCursor, TargetPortError,
    TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryAuthorization,
    TargetQueryPermission, TargetQueryResult, TargetResourceScope, TargetRetainedEventStream,
};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, CommandLifecycle, CommandOperation,
    CommandRequest, CommandResult, CommandReturnRoute, ContractVersion, Environment,
    ExternalCommand, PrincipalId, ProcessInstanceId, ReleaseId, RequestId, ResourceRef,
    RuntimeIdentity, ServiceIdentity, StationId, TargetInstanceId, UtcTimestamp,
};
use uob_management_adapter::{
    ManagementCommandAuthenticator, ManagementCommandConfiguration, ManagementReadLimits,
    ManagementRouterOptions, PrivilegedPayloadValidator, router_with_queries_and_commands,
};

#[derive(Default)]
struct CommandState {
    result: Mutex<Option<CommandResult>>,
    failure: Mutex<Option<CommandAdmissionErrorCode>>,
    admissions: AtomicUsize,
}

impl CommandAdmissionPort<Value> for CommandState {
    fn submit(&self, command: ExternalCommand<Value>) -> CommandAdmissionFuture<'_, CommandResult> {
        self.admissions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if let Some(code) = *self.failure.lock() {
                return Err(CommandAdmissionError::new(code, "fixture"));
            }
            let result = CommandResult {
                schema_version: ContractVersion::V1_INITIAL,
                correlation_id: command.request.correlation_id,
                resource: command.request.resource,
                return_route: CommandReturnRoute {
                    request_id: command.request.request_id,
                    origin: command.origin,
                },
                lifecycle: CommandLifecycle::ProtocolResponse {
                    accepted: true,
                    error: None,
                },
                recorded_at: timestamp(),
                observed_effects: vec![],
                configuration: None,
                configuration_observations: Vec::new(),
            };
            *self.result.lock() = Some(result.clone());
            Ok(result)
        })
    }
}

impl CanonicalQuerySource<Value> for CommandState {
    fn query<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<Value>> {
        Box::pin(async move {
            match query {
                TargetQuery::CommandResult(request_id) => Ok(TargetQueryResult::CommandResult(
                    self.result
                        .lock()
                        .clone()
                        .filter(|result| result.return_route.request_id == request_id),
                )),
                TargetQuery::StationSnapshots(_) => Ok(TargetQueryResult::StationSnapshots(Page {
                    items: vec![],
                    next_cursor: None::<SnapshotCursor>,
                })),
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "fixture.unsupported",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        _: uob_application::RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<Value>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "fixture.unsupported",
            ))
        })
    }
}

struct RejectPrivileged;

impl PrivilegedPayloadValidator for RejectPrivileged {
    fn validate(
        &self,
        _: &uob_contracts::PrivilegedOcppOperation<Value>,
    ) -> Result<(), &'static str> {
        Err("command.privileged_schema_invalid")
    }
}

fn origin() -> AuthenticatedCommandOrigin {
    AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("operator-a").unwrap(),
    }
}

struct CommandAuthenticator;

impl ManagementCommandAuthenticator for CommandAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedCommandOrigin> {
        let principal_id = match token {
            "operator-secret" => "operator-a",
            "other-secret" => "operator-b",
            _ => return None,
        };
        Some(AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new(principal_id).unwrap(),
        })
    }
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        station_id: StationId::new("station-a").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp() -> UtcTimestamp {
    serde_json::from_str("\"2099-01-01T00:00:00Z\"").unwrap()
}

fn router(state: Arc<CommandState>) -> axum::Router {
    let application = Application::new(ServiceIdentity {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        runtime: RuntimeIdentity {
            environment: Environment::Production,
            release_id: ReleaseId::new("release-api").unwrap(),
            release_digest: ArtifactDigest::new("sha256:api").unwrap(),
            process_instance_id: ProcessInstanceId::new("process-api").unwrap(),
        },
        selected_target_id: None,
    });
    let grant = AccessGrant::new(
        origin(),
        vec![AccessPermission::Read, AccessPermission::Control],
        vec![AccessResourceScope::Resource(resource())],
    )
    .unwrap();
    let admission: Arc<dyn CommandAdmissionPort<Value>> = Arc::new(
        ScopedCommandAdmissionPort::new(state.clone(), AccessPolicy::single(grant)),
    );
    router_with_queries_and_commands(
        application,
        state,
        TargetQueryAuthorization::new(
            TargetInstanceId::new("management-api").unwrap(),
            vec![TargetQueryPermission::CommandStatus],
            vec![TargetResourceScope::Station {
                bridge_id: BridgeId::new("bridge-api").unwrap(),
                station_id: StationId::new("station-a").unwrap(),
            }],
        ),
        ManagementReadLimits::default(),
        ManagementCommandConfiguration {
            admission,
            authenticator: Arc::new(CommandAuthenticator),
            privileged_payloads: Arc::new(RejectPrivileged),
        },
        ManagementRouterOptions::default(),
    )
}

fn request() -> CommandRequest<Value> {
    CommandRequest {
        request_id: RequestId::new("request-a").unwrap(),
        correlation_id: None,
        resource: resource(),
        operation: CommandOperation::Start {
            authorization_reference: None,
        },
        expires_at: timestamp(),
    }
}

#[tokio::test]
async fn accepted_submission_has_status_link_and_status_keeps_effects_separate() {
    let state = Arc::new(CommandState::default());
    let app = router(state);
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
    assert_eq!(value["status_url"], "/api/v1/commands/request-a");
    assert_eq!(value["result"]["lifecycle"]["stage"], "protocol_response");
    assert!(value["result"].get("observed_effects").is_none());

    let status = app
        .oneshot(
            Request::get("/api/v1/commands/request-a")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
}

#[tokio::test]
async fn persistence_failure_and_id_conflict_never_return_accepted() {
    for (failure, expected) in [
        (
            CommandAdmissionErrorCode::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            CommandAdmissionErrorCode::InvalidRequest,
            StatusCode::CONFLICT,
        ),
    ] {
        let state = Arc::new(CommandState::default());
        *state.failure.lock() = Some(failure);
        let response = router(state)
            .oneshot(
                Request::post("/api/v1/commands")
                    .header(header::AUTHORIZATION, "Bearer operator-secret")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request()).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn request_cannot_supply_trusted_origin_and_unknown_fields_fail_closed() {
    let mut value = serde_json::to_value(request()).unwrap();
    value["origin"] = serde_json::json!({
        "kind": "management",
        "principal_id": "attacker"
    });
    let response = router(Arc::new(CommandState::default()))
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["error"], "command.invalid_request");
}

#[tokio::test]
async fn missing_malformed_and_wrong_credentials_never_reach_admission() {
    let state = Arc::new(CommandState::default());
    let app = router(state.clone());
    for authorization in [
        None,
        Some("Basic operator-secret"),
        Some("Bearer"),
        Some("Bearer operator-secret extra"),
        Some("Bearer wrong-secret"),
    ] {
        let mut builder =
            Request::post("/api/v1/commands").header(header::CONTENT_TYPE, "application/json");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = app
            .clone()
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&request()).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{authorization:?}"
        );
    }
    assert_eq!(state.admissions.load(Ordering::SeqCst), 0);
    assert!(state.result.lock().is_none());
}

#[tokio::test]
async fn authenticated_submit_still_obeys_scoped_admission() {
    let state = Arc::new(CommandState::default());
    let mut outside = request();
    outside.resource.station_id = StationId::new("station-b").unwrap();
    let response = router(state.clone())
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&outside).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(state.admissions.load(Ordering::SeqCst), 0);
    assert!(state.result.lock().is_none());
}

#[tokio::test]
async fn command_status_requires_credential_and_exact_result_origin() {
    let state = Arc::new(CommandState::default());
    let app = router(state.clone());
    let accepted = app
        .clone()
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&request()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);

    for (authorization, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("Bearer wrong-secret"), StatusCode::UNAUTHORIZED),
        (Some("Bearer other-secret"), StatusCode::FORBIDDEN),
        (Some("Bearer operator-secret"), StatusCode::OK),
    ] {
        let mut builder = Request::get("/api/v1/commands/request-a");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{authorization:?}");
    }

    {
        let mut result = state.result.lock();
        result.as_mut().unwrap().resource.station_id = StationId::new("station-b").unwrap();
    }
    let response = app
        .oneshot(
            Request::get("/api/v1/commands/request-a")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
