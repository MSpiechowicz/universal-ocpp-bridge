use super::*;
use serde_json::json;
use support::fixtures::snapshot_delivery;
use uob_application::{TargetDiagnostic, TargetHealthState};
use uob_contracts::UtcTimestamp;

#[tokio::test]
async fn immediate_revision7_puback_cannot_acknowledge_pending_durable_state() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    if peer.subscriptions().is_empty() {
        peer.next_subscription().await;
    }
    submit(&mut peer, "immediate", 30).await;
    let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&submission.command.origin,
        AuthenticatedCommandOrigin::Target { principal_id, target_instance_id }
        if principal_id.as_str() == "mqtt-target:main" && target_instance_id.as_str() == "main"));
    let mut delivery = fixtures::delivery("immediate", 1024);
    let result = fixtures::result_mut(&mut delivery);
    result.return_route.origin = submission.command.origin.clone();
    // Test-only host evidence exercises result consumption, not native-command admission.
    let result = result.clone();
    target
        .host
        .try_deliver(snapshot_delivery("bridge-a", "station-a", "pending-state"))
        .unwrap();
    let pending = peer.next_publish().await;
    assert_eq!(pending.topic, "uob/v1/demo/bridge-a/state/station-a");
    submission.respond(Ok(result)).unwrap();
    let immediate = peer.next_publish().await;
    fixtures::assert_payload(&immediate.payload, 1024);
    assert_eq!(
        immediate.topic,
        "uob/v1/demo/bridge-a/results/station-a/immediate"
    );
    assert!(!immediate.retain);
    assert_eq!(immediate.qos, 1);
    peer.acknowledge(&immediate).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err()
    );
    peer.acknowledge(&pending).await;
    assert_eq!(
        report(&mut target).await.delivery_id.as_str(),
        "pending-state"
    );
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn immediate_future_foreign_origin_and_oversized_native_result_degrade_without_fallback() {
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
    if peer.subscriptions().is_empty() {
        peer.next_subscription().await;
    }
    for (packet, case) in [
        (31, "future"),
        (32, "management"),
        (33, "foreign-target"),
        (34, "large"),
        (35, "valid"),
    ] {
        submit(&mut peer, case, packet).await;
        let submission = tokio::time::timeout(Duration::from_secs(2), target.host.next_command())
            .await
            .unwrap()
            .unwrap();
        let mut delivery = fixtures::delivery(case, if case == "large" { 1024 } else { 2 });
        let result = fixtures::result_mut(&mut delivery);
        result.return_route.origin = submission.command.origin.clone();
        match case {
            "future" => {
                result.schema_version = ContractVersion {
                    major: 1,
                    revision: ContractVersion::V1_LOCAL_AUTHORIZATION_201.revision + 1,
                }
            }
            "management" => {
                result.return_route.origin = AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator").unwrap(),
                }
            }
            "foreign-target" => {
                let AuthenticatedCommandOrigin::Target {
                    target_instance_id, ..
                } = &mut result.return_route.origin
                else {
                    unreachable!()
                };
                *target_instance_id = TargetInstanceId::new("other").unwrap();
            }
            "large" => assert!(serde_json::to_vec(result).unwrap().len() > 4096),
            "valid" => {}
            _ => unreachable!(),
        }
        submission.respond(Ok(result.clone())).unwrap();
        if case == "valid" {
            let valid = peer.next_publish().await;
            fixtures::assert_payload(&valid.payload, 2);
            peer.acknowledge(&valid).await;
        } else {
            let reason = if case == "large" {
                "mqtt.payload_too_large"
            } else {
                "mqtt.canonical_identity_mismatch"
            };
            assert_degraded(&mut target, reason).await;
            quiet(&mut peer).await;
        }
    }
    shutdown(&mut target, &mut peer).await;
}

async fn assert_degraded(target: &mut RunningTarget, reason: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(target.host.next_diagnostic().await.unwrap(),
                TargetDiagnostic::Health(health) if health.state == TargetHealthState::Degraded && health.reason.as_deref() == Some(reason)) {
                break;
            }
        }
    }).await.unwrap();
}

async fn submit(peer: &mut BrokerConnection, id: &str, packet: u16) {
    let request = json!({
        "schema_version":{"major":1,"revision":0}, "request_id":id,
        "resource":{"bridge_id":"bridge-a","station_id":"station-a"},
        "operation":{"kind":"start","parameters":{}},
        "expires_at":UtcTimestamp::new(time::OffsetDateTime::now_utc() + Duration::from_secs(60))
    });
    peer.publish_command(
        &format!("uob/v1/demo/bridge-a/commands/station-a/{id}"),
        &serde_json::to_vec(&request).unwrap(),
        packet,
        false,
        false,
    )
    .await;
}
