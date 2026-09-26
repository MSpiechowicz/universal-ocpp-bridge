use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, Application,
    CanonicalQuerySource, CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture,
    CommandAdmissionPort, ScopedCommandAdmissionPort, TargetPortFuture, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryResult, TargetResourceScope,
    TargetRetainedEventStream,
};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, CommandLifecycle, CommandOperation,
    CommandRequest, CommandResult, CommandReturnRoute, Connectivity, ContractVersion, Environment,
    ExternalCommand, PayloadSchemaId, PrincipalId, PrivilegedOcppOperation, ProcessInstanceId,
    ProtocolActionName, ProtocolEdition, ReleaseId, RequestId, ResourceCapabilities, ResourceRef,
    RuntimeIdentity, ServiceIdentity, StationId, StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_management_adapter::{
    AuthenticatedEventAccess, ManagementCommandAuthenticator, ManagementCommandConfiguration,
    ManagementEventAuthenticator, ManagementEventConfiguration, ManagementEventLimits,
    ManagementReadLimits, ManagementRouterOptions, PrivilegedPayloadValidator,
    router_with_commands_and_authenticated_events,
};

#[derive(Default)]
struct State {
    existing: Mutex<Option<(ExternalCommand<Value>, CommandResult)>>,
    offered: AtomicBool,
    dispatches: AtomicUsize,
    result_queries: AtomicUsize,
}

impl CommandAdmissionPort<Value> for State {
    fn submit(&self, command: ExternalCommand<Value>) -> CommandAdmissionFuture<'_, CommandResult> {
        Box::pin(async move {
            let mut existing = self.existing.lock();
            if let Some((prior, result)) = &*existing {
                if *prior == command {
                    return Ok(result.clone());
                }
                return Err(CommandAdmissionError::new(
                    CommandAdmissionErrorCode::InvalidRequest,
                    "request ID conflict",
                ));
            }
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            let result = CommandResult {
                schema_version: ContractVersion::V1_INITIAL,
                correlation_id: command.request.correlation_id.clone(),
                resource: command.request.resource.clone(),
                return_route: CommandReturnRoute {
                    request_id: command.request.request_id.clone(),
                    origin: command.origin.clone(),
                },
                lifecycle: CommandLifecycle::ProtocolResponse {
                    accepted: true,
                    error: None,
                },
                recorded_at: timestamp(),
                observed_effects: vec![],
                configuration: None,
                configuration_observations: vec![],
            };
            *existing = Some((command, result.clone()));
            Ok(result)
        })
    }
}

impl CanonicalQuerySource<Value> for State {
    fn query<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<Value>> {
        Box::pin(async move {
            match query {
                TargetQuery::CommandResult(request_id) => {
                    self.result_queries.fetch_add(1, Ordering::SeqCst);
                    Ok(TargetQueryResult::CommandResult(
                        self.existing.lock().as_ref().and_then(|(command, result)| {
                            (command.request.request_id == request_id).then(|| result.clone())
                        }),
                    ))
                }
                TargetQuery::StationSnapshot(station) => {
                    Ok(TargetQueryResult::StationSnapshot(Some(StationSnapshot {
                        schema_version: ContractVersion::V1_INITIAL,
                        station,
                        observed_at: timestamp(),
                        connectivity: Connectivity::Disconnected,
                        capabilities: ResourceCapabilities::default(),
                        resources: vec![],
                        transactions: vec![],
                        current_values: vec![],
                    })))
                }
                _ => unreachable!("only command result and station snapshot are requested"),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        _: uob_application::RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<Value>> {
        unreachable!("retry API does not subscribe to events")
    }
}

struct Authenticator;

impl ManagementCommandAuthenticator for Authenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedCommandOrigin> {
        let principal = match token {
            "operator-secret" => "operator-a",
            "other-secret" => "operator-b",
            "schema-only-secret" => "operator-c",
            _ => return None,
        };
        Some(AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new(principal).unwrap(),
        })
    }

    fn permits_schema(&self, origin: &AuthenticatedCommandOrigin, station: &ResourceRef) -> bool {
        *station == resource()
            && matches!(origin, AuthenticatedCommandOrigin::Management { principal_id }
                if principal_id.as_str() == "operator-a" || principal_id.as_str() == "operator-c")
    }
}

struct ReadAuthenticator;

impl ManagementEventAuthenticator for ReadAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedEventAccess> {
        (token == "reader-secret").then(|| AuthenticatedEventAccess {
            authorization: TargetQueryAuthorization::new(
                TargetInstanceId::new("management-read").unwrap(),
                vec![
                    TargetQueryPermission::StationSnapshots,
                    TargetQueryPermission::CommandStatus,
                    TargetQueryPermission::RetainedEvents,
                ],
                vec![TargetResourceScope::Station {
                    bridge_id: resource().bridge_id,
                    station_id: resource().station_id,
                }],
            ),
            default_resource: resource(),
        })
    }
}

struct Validator(Arc<State>);

impl PrivilegedPayloadValidator for Validator {
    fn validate(&self, operation: &PrivilegedOcppOperation<Value>) -> Result<(), &'static str> {
        if operation.action.as_str() == "Reset"
            && operation.payload_schema.as_str() == "urn:test:reset"
            && operation.payload == json!({"type": "Soft"})
        {
            Ok(())
        } else {
            Err("command.privileged_schema_invalid")
        }
    }

    fn offers(
        &self,
        _: &StationSnapshot,
        _: &ResourceRef,
        _: &PrivilegedOcppOperation<Value>,
    ) -> bool {
        self.0.offered.load(Ordering::SeqCst)
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

fn operator() -> AuthenticatedCommandOrigin {
    AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("operator-a").unwrap(),
    }
}

fn timestamp() -> UtcTimestamp {
    serde_json::from_str("\"2099-01-01T00:00:00Z\"").unwrap()
}

fn request(id: &str) -> CommandRequest<Value> {
    CommandRequest {
        request_id: RequestId::new(id).unwrap(),
        correlation_id: None,
        resource: resource(),
        operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp16j,
            action: ProtocolActionName::new("Reset").unwrap(),
            payload_schema: PayloadSchemaId::new("urn:test:reset").unwrap(),
            payload: json!({"type": "Soft"}),
        }),
        expires_at: timestamp(),
    }
}

fn router(state: Arc<State>) -> axum::Router {
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
        operator(),
        vec![AccessPermission::PrivilegedControl],
        vec![AccessResourceScope::Resource(resource())],
    )
    .unwrap();
    let admission: Arc<dyn CommandAdmissionPort<Value>> = Arc::new(
        ScopedCommandAdmissionPort::new(state.clone(), AccessPolicy::single(grant)),
    );
    router_with_commands_and_authenticated_events(
        application,
        state.clone(),
        ManagementReadLimits::default(),
        ManagementCommandConfiguration {
            admission,
            authenticator: Arc::new(Authenticator),
            privileged_payloads: Arc::new(Validator(state)),
        },
        ManagementEventConfiguration {
            authenticator: Arc::new(ReadAuthenticator),
            limits: ManagementEventLimits::default(),
        },
        ManagementRouterOptions::default(),
    )
}

async fn submit(
    app: &axum::Router,
    token: Option<&str>,
    request: &CommandRequest<Value>,
) -> (StatusCode, Value) {
    let mut builder =
        Request::post("/api/v1/commands").header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .body(Body::from(serde_json::to_vec(request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
    (status, body)
}

#[tokio::test]
async fn exact_retry_skips_withdrawn_offer_but_conflict_and_fresh_request_do_not() {
    let state = Arc::new(State::default());
    state.offered.store(true, Ordering::SeqCst);
    let app = router(state.clone());
    let command = request("request-a");

    let (status, original) = submit(&app, Some("operator-secret"), &command).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(state.dispatches.load(Ordering::SeqCst), 1);
    state.offered.store(false, Ordering::SeqCst);

    let (status, retried) = submit(&app, Some("operator-secret"), &command).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(retried, original);
    assert_eq!(state.dispatches.load(Ordering::SeqCst), 1);

    let (status, fresh) = submit(&app, Some("operator-secret"), &request("request-b")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fresh["error"], "command.unsupported_schema");

    let mut conflict = command.clone();
    conflict.expires_at = serde_json::from_str("\"2098-01-01T00:00:00Z\"").unwrap();
    let (status, result) = submit(&app, Some("operator-secret"), &conflict).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["error"], "command.request_conflict");
    assert_eq!(state.dispatches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unauthorized_and_malformed_retries_never_use_durable_result() {
    let state = Arc::new(State::default());
    state.offered.store(true, Ordering::SeqCst);
    let app = router(state.clone());
    let command = request("request-a");
    assert_eq!(
        submit(&app, Some("operator-secret"), &command).await.0,
        StatusCode::ACCEPTED
    );
    state.offered.store(false, Ordering::SeqCst);
    let prior_queries = state.result_queries.load(Ordering::SeqCst);

    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("other-secret"), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(submit(&app, token, &command).await.0, expected);
    }
    let mut invalid = command.clone();
    if let CommandOperation::Ocpp(operation) = &mut invalid.operation {
        operation.payload = json!({"type": "Unknown"});
    }
    assert_eq!(
        submit(&app, Some("operator-secret"), &invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(state.result_queries.load(Ordering::SeqCst), prior_queries);
    assert_eq!(state.dispatches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn schema_grant_does_not_expose_another_principals_retry_or_override_admission() {
    let state = Arc::new(State::default());
    state.offered.store(true, Ordering::SeqCst);
    let app = router(state.clone());
    let command = request("request-a");
    assert_eq!(
        submit(&app, Some("operator-secret"), &command).await.0,
        StatusCode::ACCEPTED
    );

    // This principal can inspect the station schema, but the scoped admission policy does
    // not grant it privileged control. Its matching request ID is not a matching origin.
    let (status, denied) = submit(&app, Some("schema-only-secret"), &command).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(denied["error"], "command.unauthorized");

    state.offered.store(false, Ordering::SeqCst);
    let (status, denied) = submit(&app, Some("schema-only-secret"), &command).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(denied["error"], "command.unsupported_schema");
    assert_eq!(state.dispatches.load(Ordering::SeqCst), 1);
}
