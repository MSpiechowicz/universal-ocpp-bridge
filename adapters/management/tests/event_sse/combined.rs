use super::*;
use serde_json::Value;
use uob_application::{
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandOperation, CommandRequest, ExternalCommand, PrincipalId,
    PrivilegedOcppOperation, RequestId,
};
use uob_management_adapter::{
    ManagementCommandConfiguration, PrivilegedPayloadValidator,
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
            origin: AuthenticatedCommandOrigin::Management {
                principal_id: PrincipalId::new("operator-a").unwrap(),
            },
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
    let response = router
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&command).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}
