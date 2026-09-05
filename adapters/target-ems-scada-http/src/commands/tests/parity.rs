use std::sync::Arc;

use axum::{Router, body::Body, http::StatusCode};
use serde_json::{Value, json};
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
    ManagementCommandConfiguration, ManagementReadLimits, ManagementRouterOptions,
    PrivilegedPayloadValidator, router_with_queries_and_commands,
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

fn management(harness: &Harness) -> Router {
    let origin = AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("management-operator").unwrap(),
    };
    let grant = AccessGrant::new(
        origin.clone(),
        vec![AccessPermission::Read, AccessPermission::Control],
        vec![AccessResourceScope::Bridge(
            BridgeId::new("site-01").unwrap(),
        )],
    )
    .unwrap();
    router_with_queries_and_commands(
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
        TargetQueryAuthorization::new(
            TargetInstanceId::new("management").unwrap(),
            vec![TargetQueryPermission::CommandStatus],
            vec![TargetResourceScope::Station {
                bridge_id: BridgeId::new("site-01").unwrap(),
                station_id: StationId::new("station-a").unwrap(),
            }],
        ),
        ManagementReadLimits::default(),
        ManagementCommandConfiguration {
            admission: Arc::new(ScopedCommandAdmissionPort::new(
                harness.coordinator.clone(),
                AccessPolicy::single(grant),
            )),
            origin,
            privileged_payloads: Arc::new(RejectPrivileged),
        },
        ManagementRouterOptions::default(),
    )
}

#[tokio::test]
async fn http_and_management_use_equivalent_durable_commands_and_keep_origins_separate() {
    let harness = Harness::new();
    let (_, http) = post(harness.router(), "operator", payload("http", "station-a")).await;
    let (status, management) = send(
        management(&harness),
        "POST",
        "/api/v1/commands",
        "unused",
        Body::from(payload("management", "station-a").to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{management}");
    assert_eq!(management["result"]["resource"], http["result"]["resource"]);
    assert_eq!(
        management["result"]["lifecycle"],
        http["result"]["lifecycle"]
    );
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
