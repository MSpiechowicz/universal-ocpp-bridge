mod support;

use std::time::Duration;

use serde_json::{Value, json};
use time::OffsetDateTime;
use uob_application::{TargetPortError, TargetPortErrorCode};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandLifecycle, CommandResult, CommandReturnRoute,
    ContractVersion, UtcTimestamp,
};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

use support::{
    Publication, TestBroker,
    fixtures::{resource, snapshot_delivery, timestamp},
    harness::{standard_capacities, start_target},
};

#[tokio::test]
async fn subscribes_only_to_the_trusted_command_namespace_and_attaches_trusted_origin() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "trusted/bridge+#",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;
    ensure_subscription(&mut peer, "uob/v1/demo/trusted%2Fbridge%2B%23/commands/+/+").await;

    let payload = command("trusted/bridge+#", "station/+#", "request/+", future());
    peer.publish_command(
        "uob/v1/demo/trusted%2Fbridge%2B%23/commands/station%2F%2B%23/request%2F%2B",
        &serde_json::to_vec(&payload).unwrap(),
        7,
        false,
        false,
    )
    .await;

    let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
        .await
        .expect("command admission timeout")
        .expect("command admission");
    assert_eq!(
        submission.command.request.resource,
        resource("trusted/bridge+#", "station/+#")
    );
    assert!(matches!(
        &submission.command.origin,
        AuthenticatedCommandOrigin::Target { target_instance_id, principal_id }
            if target_instance_id.as_str() == "main"
                && principal_id.as_str() == "mqtt-target:main"
    ));
    let result = admitted(&submission.command);
    submission
        .respond(Ok(result.clone()))
        .expect("host response");
    let published = peer.next_publish().await;
    assert_result(&published, "request/+", &CommandLifecycle::Admitted);
    assert_eq!(
        serde_json::from_slice::<CommandResult>(&published.payload).unwrap(),
        result
    );
    peer.acknowledge(&published).await;

    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn rejects_retained_expired_and_spoofed_commands_before_dispatch() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;
    ensure_subscription(&mut peer, "uob/v1/demo/bridge-a/commands/+/+").await;

    let retained = command("bridge-a", "station-a", "retained", future());
    peer.publish_command(
        "uob/v1/demo/bridge-a/commands/station-a/retained",
        &serde_json::to_vec(&retained).unwrap(),
        8,
        true,
        false,
    )
    .await;
    let result = peer.next_publish().await;
    assert_result_code(
        &result,
        "retained",
        "policy_rejected",
        "mqtt.retained_command",
    );
    peer.acknowledge(&result).await;

    let expired = command(
        "bridge-a",
        "station-a",
        "expired",
        OffsetDateTime::UNIX_EPOCH,
    );
    peer.publish_command(
        "uob/v1/demo/bridge-a/commands/station-a/expired",
        &serde_json::to_vec(&expired).unwrap(),
        9,
        false,
        false,
    )
    .await;
    let result = peer.next_publish().await;
    assert_result_code(&result, "expired", "expired", "mqtt.command_expired");
    peer.acknowledge(&result).await;

    let mut spoofed = command("bridge-a", "station-a", "spoofed", future());
    spoofed["origin"] = json!({
        "kind": "management",
        "principal_id": "admin"
    });
    spoofed["environment"] = json!("production");
    peer.publish_command(
        "uob/v1/demo/bridge-a/commands/station-a/spoofed",
        &serde_json::to_vec(&spoofed).unwrap(),
        10,
        false,
        false,
    )
    .await;

    assert!(
        tokio::time::timeout(Duration::from_millis(150), target.host.next_command())
            .await
            .is_err(),
        "retained, expired, and spoofed payloads must not reach host admission"
    );
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn qos1_redelivery_reuses_identity_and_changed_content_returns_stable_conflict() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;
    ensure_subscription(&mut peer, "uob/v1/demo/bridge-a/commands/+/+").await;
    let topic = "uob/v1/demo/bridge-a/commands/station-a/idempotent";
    let payload = command("bridge-a", "station-a", "idempotent", future());

    peer.publish_command(
        topic,
        &serde_json::to_vec(&payload).unwrap(),
        11,
        false,
        false,
    )
    .await;
    let first = target.host.next_command().await.expect("first admission");
    let admitted = admitted(&first.command);
    first.respond(Ok(admitted.clone())).expect("first result");
    let first_result = peer.next_publish().await;
    assert_eq!(
        serde_json::from_slice::<CommandResult>(&first_result.payload).unwrap(),
        admitted
    );
    peer.acknowledge(&first_result).await;

    peer.publish_command(
        topic,
        &serde_json::to_vec(&payload).unwrap(),
        12,
        false,
        true,
    )
    .await;
    let duplicate = target
        .host
        .next_command()
        .await
        .expect("duplicate admission");
    assert_eq!(duplicate.command.request, payload_request(&payload));
    duplicate
        .respond(Ok(admitted.clone()))
        .expect("idempotent replay result");
    let duplicate_result = peer.next_publish().await;
    assert_eq!(
        serde_json::from_slice::<CommandResult>(&duplicate_result.payload).unwrap(),
        admitted,
        "application idempotency result must remain stable"
    );
    peer.acknowledge(&duplicate_result).await;

    let mut conflicting = payload;
    conflicting["operation"]["parameters"]["authorization_reference"] = json!("changed");
    peer.publish_command(
        topic,
        &serde_json::to_vec(&conflicting).unwrap(),
        13,
        false,
        true,
    )
    .await;
    let conflict = target
        .host
        .next_command()
        .await
        .expect("conflict admission");
    conflict
        .respond(Err(TargetPortError::new(
            TargetPortErrorCode::InvalidRequest,
            "command.request_conflict",
        )))
        .expect("conflict result");
    let conflict_result = peer.next_publish().await;
    assert_result_code(
        &conflict_result,
        "idempotent",
        "invalid_parameters",
        "mqtt.command_request_conflict",
    );
    peer.acknowledge(&conflict_result).await;

    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn inbound_command_reaches_admission_while_outbound_delivery_waits_for_puback() {
    let broker = TestBroker::bind().await;
    let runtime = MqttRuntimeOptions {
        request_capacity: 1,
        maximum_in_flight_deliveries: 1,
        ..MqttRuntimeOptions::default()
    };
    let mut target = start_target(&broker, "bridge-a", runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;
    ensure_subscription(&mut peer, "uob/v1/demo/bridge-a/commands/+/+").await;

    target
        .host
        .try_deliver(snapshot_delivery("bridge-a", "station-a", "slow-state"))
        .expect("outbound delivery");
    let slow = peer.next_publish().await;
    assert!(slow.topic.contains("/state/"));

    let payload = command("bridge-a", "station-a", "during-slow", future());
    peer.publish_command(
        "uob/v1/demo/bridge-a/commands/station-a/during-slow",
        &serde_json::to_vec(&payload).unwrap(),
        14,
        false,
        false,
    )
    .await;
    let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
        .await
        .expect("slow outbound must not block command ingress")
        .expect("command admission");
    let result = admitted(&submission.command);
    submission.respond(Ok(result)).expect("command result");

    peer.acknowledge(&slow).await;
    let published = peer.next_publish().await;
    assert_result(&published, "during-slow", &CommandLifecycle::Admitted);
    peer.acknowledge(&published).await;
    target
        .host
        .next_report()
        .await
        .expect("slow delivery report");

    shutdown(&mut target, &mut peer).await;
}

fn command(bridge: &str, station: &str, request_id: &str, expires: OffsetDateTime) -> Value {
    json!({
        "schema_version": { "major": 1, "revision": 0 },
        "request_id": request_id,
        "correlation_id": format!("correlation-{request_id}"),
        "resource": {
            "bridge_id": bridge,
            "station_id": station
        },
        "operation": {
            "kind": "start",
            "parameters": { "authorization_reference": "local-auth" }
        },
        "expires_at": UtcTimestamp::new(expires)
    })
}

fn future() -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::from_secs(60)
}

fn admitted<P>(command: &uob_contracts::ExternalCommand<P>) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Admitted,
        recorded_at: timestamp(),
        observed_effects: vec![],
    }
}

fn payload_request(payload: &Value) -> uob_contracts::CommandRequest<()> {
    serde_json::from_value(json!({
        "request_id": payload["request_id"],
        "correlation_id": payload["correlation_id"],
        "resource": payload["resource"],
        "operation": payload["operation"],
        "expires_at": payload["expires_at"]
    }))
    .expect("command request")
}

async fn acknowledge_online(peer: &mut support::BrokerConnection) {
    let online = peer.next_publish().await;
    assert_eq!(online.topic.split('/').next_back(), Some("availability"));
    peer.acknowledge(&online).await;
}

async fn ensure_subscription(peer: &mut support::BrokerConnection, expected: &str) {
    if peer.subscriptions().is_empty() {
        peer.next_subscription().await;
    }
    assert_eq!(peer.subscriptions(), &[expected.to_owned()]);
}

fn assert_result(publication: &Publication, request_id: &str, lifecycle: &CommandLifecycle) {
    assert_eq!(publication.qos, 1);
    assert!(!publication.retain);
    assert!(publication.topic.contains("/results/"));
    let result: CommandResult = serde_json::from_slice(&publication.payload).unwrap();
    assert_eq!(result.return_route.request_id.as_str(), request_id);
    assert_eq!(&result.lifecycle, lifecycle);
}

fn assert_result_code(publication: &Publication, request_id: &str, code: &str, detail: &str) {
    assert!(publication.topic.ends_with(&format!("/{request_id}")));
    let value: Value = serde_json::from_slice(&publication.payload).unwrap();
    assert_eq!(value["lifecycle"]["stage"], "rejected");
    assert_eq!(value["lifecycle"]["error"]["code"], code);
    assert_eq!(value["lifecycle"]["error"]["detail"], detail);
}

async fn shutdown(
    target: &mut support::harness::RunningTarget,
    peer: &mut support::BrokerConnection,
) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .expect("shutdown timeout")
        .expect("join")
        .expect("target shutdown");
}
