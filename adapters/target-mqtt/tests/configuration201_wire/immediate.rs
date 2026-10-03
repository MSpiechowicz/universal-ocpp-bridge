use super::*;
use support::fixtures::snapshot_delivery;
use uob_application::{TargetDiagnostic, TargetHealthState};
use uob_contracts::UtcTimestamp;

#[tokio::test]
async fn immediate_configuration_puback_does_not_acknowledge_pending_durable_state() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    ensure_subscription(&mut peer).await;
    submit(&mut peer, "immediate", 40).await;
    let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&submission.command.origin, AuthenticatedCommandOrigin::Target { target_instance_id, principal_id }
        if target_instance_id.as_str() == "main" && principal_id.as_str() == "mqtt-target:main")
    );
    let evidence = fixtures::network("Accepted");
    let mut delivery = fixtures::delivery("immediate", evidence.clone());
    let result = fixtures::result_mut(&mut delivery);
    result.return_route.origin = submission.command.origin.clone();
    let result = result.clone();
    target
        .host
        .try_deliver(snapshot_delivery("bridge-a", "station-a", "pending-state"))
        .unwrap();
    let pending = peer.next_publish().await;
    assert_eq!(pending.topic, "uob/v1/demo/bridge-a/state/station-a");
    // Authoritative test-only host evidence exercises consumption, not native ingress.
    submission.respond(Ok(result)).unwrap();
    let immediate = peer.next_publish().await;
    assert_publication(&immediate, "immediate", &evidence);
    assert_ne!(pending.packet_id, immediate.packet_id);
    peer.acknowledge(&immediate).await;
    assert_no_report(&mut target).await;
    peer.acknowledge(&pending).await;
    let completed = report(&mut target).await;
    assert_eq!(completed.delivery_id.as_str(), "pending-state");
    assert_broker_receipt(completed.outcome);
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn immediate_configuration_foreign_future_and_oversized_results_do_not_publish_fallbacks() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions {
            maximum_message_bytes: 4096,
            ..MqttRuntimeOptions::default()
        },
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    ensure_subscription(&mut peer).await;
    for (packet, case) in [
        (41, "future"),
        (42, "management"),
        (43, "foreign-origin"),
        (44, "foreign-bridge"),
        (45, "large"),
        (46, "valid"),
    ] {
        submit(&mut peer, case, packet).await;
        let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
            .await
            .unwrap()
            .unwrap();
        let evidence = if case == "large" {
            fixtures::variables(128)
        } else {
            fixtures::variables(6)
        };
        let mut delivery = fixtures::delivery(case, evidence.clone());
        let result = fixtures::result_mut(&mut delivery);
        result.return_route.origin = submission.command.origin.clone();
        match case {
            "future" => {
                result.schema_version = ContractVersion {
                    major: 1,
                    revision: ContractVersion::V1_CONFIGURATION_201.revision + 1,
                }
            }
            "management" => {
                result.return_route.origin = AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator").unwrap(),
                }
            }
            "foreign-origin" => {
                let AuthenticatedCommandOrigin::Target {
                    target_instance_id, ..
                } = &mut result.return_route.origin
                else {
                    unreachable!()
                };
                *target_instance_id = TargetInstanceId::new("other").unwrap();
            }
            "foreign-bridge" => result.resource.bridge_id = BridgeId::new("other").unwrap(),
            "large" => assert!(serde_json::to_vec(result).unwrap().len() > 4096),
            "valid" => {}
            _ => unreachable!(),
        }
        submission.respond(Ok(result.clone())).unwrap();
        if case == "valid" {
            let publication = peer.next_publish().await;
            assert_publication(&publication, case, &evidence);
            peer.acknowledge(&publication).await;
        } else {
            degraded(
                &mut target,
                if case == "large" {
                    "mqtt.payload_too_large"
                } else {
                    "mqtt.canonical_identity_mismatch"
                },
            )
            .await;
            quiet(&mut peer).await;
        }
    }
    shutdown(&mut target, &mut peer).await;
}

async fn degraded(target: &mut RunningTarget, reason: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(target.host.next_diagnostic().await.unwrap(), TargetDiagnostic::Health(health)
                if health.state == TargetHealthState::Degraded && health.reason.as_deref() == Some(reason)) {
                break;
            }
        }
    }).await.unwrap();
}

async fn ensure_subscription(peer: &mut BrokerConnection) {
    if peer.subscriptions().is_empty() {
        peer.next_subscription().await;
    }
}

async fn submit(peer: &mut BrokerConnection, id: &str, packet: u16) {
    let request = json!({"schema_version":{"major":1,"revision":0},"request_id":id,
        "resource":{"bridge_id":"bridge-a","station_id":"station-a"},
        "operation":{"kind":"start","parameters":{}},
        "expires_at":UtcTimestamp::new(time::OffsetDateTime::now_utc() + Duration::from_secs(60))});
    peer.publish_command(
        &format!("uob/v1/demo/bridge-a/commands/station-a/{id}"),
        &serde_json::to_vec(&request).unwrap(),
        packet,
        false,
        false,
    )
    .await;
}
