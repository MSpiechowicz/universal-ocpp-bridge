use super::*;
use serde_json::Value;
use uob_application::{
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandLifecycle, CommandOperation, CommandRequest, CommandResult,
    CommandReturnRoute, ExternalCommand, PrincipalId, PrivilegedOcppOperation, RequestId,
};
use uob_management_adapter::{
    ManagementCommandAuthenticator, ManagementCommandConfiguration, PrivilegedPayloadValidator,
    router_with_commands_and_authenticated_events,
};

struct BusyAdmission;

impl CommandAdmissionPort<Value> for BusyAdmission {
    fn submit(
        &self,
        _: ExternalCommand<Value>,
    ) -> CommandAdmissionFuture<'_, uob_contracts::CommandResult> {
        Box::pin(async {
            Err(CommandAdmissionError::new(
                CommandAdmissionErrorCode::Busy,
                "fixture.busy",
            ))
        })
    }
}

struct AcceptPayloadShape;

impl PrivilegedPayloadValidator for AcceptPayloadShape {
    fn validate(&self, _: &PrivilegedOcppOperation<Value>) -> Result<(), &'static str> {
        Ok(())
    }
}

struct CommandAuthenticator;

impl ManagementCommandAuthenticator for CommandAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedCommandOrigin> {
        (token == "command-secret").then(|| AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("operator-a").unwrap(),
        })
    }
}

#[tokio::test]
async fn combined_router_keeps_both_command_and_event_routes_enabled() {
    let application = application();
    let resource = station_resource("station-a");
    let source = Arc::new(EventSource::default());
    let authenticator = Arc::new(TokenAuthenticator {
        access: AuthenticatedEventAccess {
            authorization: authorization(&resource),
            default_resource: resource.clone(),
        },
    });
    let router = router_with_commands_and_authenticated_events(
        application,
        source.clone(),
        ManagementReadLimits::default(),
        ManagementCommandConfiguration {
            admission: Arc::new(BusyAdmission),
            authenticator: Arc::new(CommandAuthenticator),
            privileged_payloads: Arc::new(AcceptPayloadShape),
        },
        ManagementEventConfiguration {
            authenticator,
            limits: ManagementEventLimits::default(),
        },
        ManagementRouterOptions::default(),
    );

    let events = router
        .clone()
        .oneshot(event_request("/api/v1/events", None))
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(source.subscription_calls.load(Ordering::SeqCst), 1);

    let command: CommandRequest<Value> = CommandRequest {
        request_id: RequestId::new("combined-request").unwrap(),
        correlation_id: None,
        resource,
        operation: CommandOperation::Start {
            authorization_reference: None,
        },
        expires_at: timestamp(),
    };
    for authorization in [None, Some("Bearer reader-secret")] {
        let mut builder =
            Request::post("/api/v1/commands").header(header::CONTENT_TYPE, "application/json");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = router
            .clone()
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&command).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let response = router
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::AUTHORIZATION, "Bearer command-secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&command).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn combined_status_requires_event_bearer_and_command_status_grant() {
    let resource = station_resource("station-a");
    let source = Arc::new(EventSource {
        command_result: Some(CommandResult {
            schema_version: ContractVersion::V1_INITIAL,
            correlation_id: None,
            resource: resource.clone(),
            return_route: CommandReturnRoute {
                request_id: RequestId::new("combined-request").unwrap(),
                origin: AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator-a").unwrap(),
                },
            },
            lifecycle: CommandLifecycle::Admitted,
            recorded_at: timestamp(),
            observed_effects: vec![],
            configuration: None,
            configuration_observations: vec![],
        }),
        ..EventSource::default()
    });
    let read_access = AuthenticatedEventAccess {
        authorization: TargetQueryAuthorization::new(
            TargetInstanceId::new("management-events").unwrap(),
            vec![TargetQueryPermission::CommandStatus],
            vec![TargetResourceScope::Station {
                bridge_id: resource.bridge_id.clone(),
                station_id: resource.station_id.clone(),
            }],
        ),
        default_resource: resource,
    };
    let router = router_with_commands_and_authenticated_events(
        application(),
        source.clone(),
        ManagementReadLimits::default(),
        ManagementCommandConfiguration {
            admission: Arc::new(BusyAdmission),
            authenticator: Arc::new(CommandAuthenticator),
            privileged_payloads: Arc::new(AcceptPayloadShape),
        },
        ManagementEventConfiguration {
            authenticator: Arc::new(TokenAuthenticator {
                access: read_access,
            }),
            limits: ManagementEventLimits::default(),
        },
        ManagementRouterOptions::default(),
    );

    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("command-secret"), StatusCode::UNAUTHORIZED),
        (Some("reader-secret"), StatusCode::OK),
    ] {
        let mut builder = Request::get("/api/v1/commands/combined-request");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{token:?}");
    }
    assert_eq!(source.query_calls.load(Ordering::SeqCst), 1);
}
