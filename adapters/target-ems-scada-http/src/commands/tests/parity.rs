use std::sync::{Arc, atomic::Ordering};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, Application,
    CommandAdmissionPort, OperationalStore, ScopedCommandAdmissionPort, TargetQueryAuthorization,
    TargetQueryPermission, TargetResourceScope,
};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, CommandRequest, Environment, EventId,
    EventType, ExternalCommand, ObservedCommandEffect, PrincipalId, ProcessInstanceId, ReleaseId,
    RequestId, RuntimeIdentity, ServiceIdentity, StationId, TargetInstanceId, UtcTimestamp,
};
use uob_management_adapter::{
    AuthenticatedEventAccess, ManagementCommandAuthenticator, ManagementCommandConfiguration,
    ManagementEventAuthenticator, ManagementEventConfiguration, ManagementEventLimits,
    ManagementReadLimits, ManagementRouterOptions, PrivilegedPayloadValidator,
    router_with_commands_and_authenticated_events,
};

use super::support::{Harness, Source, payload, post, send};

struct RejectPrivileged;
impl PrivilegedPayloadValidator for RejectPrivileged {
    fn validate(
        &self,
        _: &uob_contracts::PrivilegedOcppOperation<Value>,
    ) -> Result<(), &'static str> {
        Err("not_granted")
    }
}

struct ManagementAuthenticator;

impl ManagementCommandAuthenticator for ManagementAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedCommandOrigin> {
        (token == "management-secret").then(|| AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("management-operator").unwrap(),
        })
    }
}

struct ReadAuthenticator;

impl ManagementEventAuthenticator for ReadAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedEventAccess> {
        (token == "management-read-secret").then(|| AuthenticatedEventAccess {
            authorization: TargetQueryAuthorization::new(
                TargetInstanceId::new("management-read").unwrap(),
                vec![
                    TargetQueryPermission::StationSnapshots,
                    TargetQueryPermission::CommandStatus,
                    TargetQueryPermission::RetainedEvents,
                ],
                vec![TargetResourceScope::Station {
                    bridge_id: BridgeId::new("site-01").unwrap(),
                    station_id: StationId::new("station-a").unwrap(),
                }],
            ),
            default_resource: uob_contracts::ResourceRef {
                bridge_id: BridgeId::new("site-01").unwrap(),
                station_id: StationId::new("station-a").unwrap(),
                resource: None,
                native_protocol_reference: None,
            },
        })
    }
}

fn management(harness: &Harness) -> Router {
    let origin = AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("management-operator").unwrap(),
    };
    let grant = AccessGrant::new(
        origin.clone(),
        vec![AccessPermission::Control],
        vec![AccessResourceScope::Bridge(
            BridgeId::new("site-01").unwrap(),
        )],
    )
    .unwrap();
    router_with_commands_and_authenticated_events(
        Application::new(ServiceIdentity {
            bridge_id: BridgeId::new("site-01").unwrap(),
            runtime: RuntimeIdentity {
                environment: Environment::Demo,
                release_id: ReleaseId::new("test").unwrap(),
                release_digest: ArtifactDigest::new("sha256:test").unwrap(),
                process_instance_id: ProcessInstanceId::new("test").unwrap(),
            },
            selected_target_id: Some(TargetInstanceId::new("main").unwrap()),
        }),
        Arc::new(Source(harness.store.clone())),
        ManagementReadLimits::default(),
        ManagementCommandConfiguration {
            admission: Arc::new(ScopedCommandAdmissionPort::new(
                harness.coordinator.clone(),
                AccessPolicy::single(grant),
            )),
            authenticator: Arc::new(ManagementAuthenticator),
            privileged_payloads: Arc::new(RejectPrivileged),
        },
        ManagementEventConfiguration {
            authenticator: Arc::new(ReadAuthenticator),
            limits: ManagementEventLimits::default(),
        },
        ManagementRouterOptions::default(),
    )
}

async fn assert_management_read_access(harness: &Harness, management_result: &Value) {
    let read_url = "/api/v1/commands/management";
    let missing_read = management(harness)
        .oneshot(Request::get(read_url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing_read.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(
            management(harness),
            "GET",
            read_url,
            "management-secret",
            Body::empty(),
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (read_status, read_result) = send(
        management(harness),
        "GET",
        read_url,
        "management-read-secret",
        Body::empty(),
    )
    .await;
    assert_eq!(read_status, StatusCode::OK);
    assert_eq!(read_result, management_result["result"]);

    let missing_station_read = management(harness)
        .oneshot(
            Request::get("/api/v1/stations/station-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_station_read.status(), StatusCode::UNAUTHORIZED);
    for (station, token, expected) in [
        ("station-a", "management-secret", StatusCode::UNAUTHORIZED),
        ("station-b", "management-read-secret", StatusCode::FORBIDDEN),
    ] {
        let url = format!("/api/v1/stations/{station}");
        assert_eq!(
            send(management(harness), "GET", &url, token, Body::empty())
                .await
                .0,
            expected
        );
    }
}

#[tokio::test]
async fn http_and_management_use_equivalent_durable_commands_and_keep_origins_separate() {
    let harness = Harness::new();
    let (_, http) = post(harness.router(), "operator", payload("http", "station-a")).await;
    let (status, management_result) = send(
        management(&harness),
        "POST",
        "/api/v1/commands",
        "management-secret",
        Body::from(payload("management", "station-a").to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{management_result}");
    assert_eq!(
        management_result["result"]["resource"],
        http["result"]["resource"]
    );
    assert_eq!(
        management_result["result"]["lifecycle"],
        http["result"]["lifecycle"]
    );
    assert_management_read_access(&harness, &management_result).await;
    for id in ["http", "management"] {
        let stored = harness
            .store
            .command_by_request_id(RequestId::new(id).unwrap())
            .await
            .unwrap()
            .unwrap();
        let expected: CommandRequest<Value> =
            serde_json::from_value(payload(id, "station-a")).unwrap();
        assert_eq!(stored.operation, expected.operation);
        assert_eq!(stored.resource, expected.resource);
        assert_eq!(stored.expires_at, expected.expires_at);
    }
    // Knowing an ID is insufficient to read a different command surface's state.
    assert_eq!(
        send(
            harness.router(),
            "GET",
            "/bridge/v1/commands/management",
            "operator",
            Body::empty()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    let request = serde_json::from_value(payload("other-target", "station-a")).unwrap();
    harness
        .coordinator
        .submit(ExternalCommand::authenticated(
            request,
            AuthenticatedCommandOrigin::Target {
                target_instance_id: TargetInstanceId::new("other").unwrap(),
                principal_id: PrincipalId::new("operator").unwrap(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        send(
            harness.router(),
            "GET",
            "/bridge/v1/commands/other-target",
            "operator",
            Body::empty()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn management_command_without_valid_bearer_is_not_persisted_or_sent() {
    let harness = Harness::new();
    for (id, credential) in [
        ("missing-management", None),
        ("wrong-management", Some("Bearer wrong")),
    ] {
        let mut builder =
            Request::post("/api/v1/commands").header("content-type", "application/json");
        if let Some(credential) = credential {
            builder = builder.header("authorization", credential);
        }
        let response = management(&harness)
            .oneshot(
                builder
                    .body(Body::from(payload(id, "station-a").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            harness
                .store
                .command_by_request_id(RequestId::new(id).unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(harness.stations.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn later_observed_effects_are_returned_without_rewriting_protocol_acceptance() {
    let harness = Harness::new();
    let (status, accepted) =
        post(harness.router(), "operator", payload("effect", "station-a")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let effect = ObservedCommandEffect {
        event_id: EventId::new("observed-event").unwrap(),
        event_type: EventType::new("transaction.started").unwrap(),
        observed_at: UtcTimestamp::new(time::OffsetDateTime::now_utc()),
    };
    harness
        .coordinator
        .reconcile_observed_effect(RequestId::new("effect").unwrap(), effect.clone())
        .await
        .unwrap();
    let (status, current) = send(
        harness.router(),
        "GET",
        "/bridge/v1/commands/effect",
        "operator",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["lifecycle"], accepted["result"]["lifecycle"]);
    assert_eq!(current["observed_effects"], json!([effect]));
}
