#[path = "charging_profile201_wire/fixtures.rs"]
mod fixtures;
#[path = "charging_profile201_wire/immediate.rs"]
mod immediate;
mod support;

use std::time::Duration;
use support::{
    BrokerConnection, TestBroker,
    harness::{RunningTarget, standard_capacities, start_target},
};
use uob_application::{DeliveryOutcome, DeliveryReport};
use uob_contracts::{AuthenticatedCommandOrigin, ContractVersion, PrincipalId, TargetInstanceId};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

#[tokio::test]
async fn revision7_full_native_evidence_waits_for_its_correlated_durable_puback() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    target
        .host
        .try_deliver(fixtures::delivery("full", 1024))
        .unwrap();
    let full = peer.next_publish().await;
    fixtures::assert_payload(&full.payload, 1024);
    assert_eq!(full.topic, "uob/v1/demo/bridge-a/results/station-a/full");
    assert_eq!(full.qos, 1);
    assert!(!full.retain);
    target
        .host
        .try_deliver(fixtures::clear_delivery("second"))
        .unwrap();
    let second = peer.next_publish().await;
    fixtures::assert_clear_payload(&second.payload);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err()
    );
    peer.acknowledge(&second).await;
    let completed = report(&mut target).await;
    assert_eq!(completed.delivery_id.as_str(), "second");
    assert!(
        matches!(completed.outcome, DeliveryOutcome::Acknowledged { scope, .. } if scope.0 == "mqtt.broker_received")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err()
    );
    peer.acknowledge(&full).await;
    assert_eq!(report(&mut target).await.delivery_id.as_str(), "full");
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn future_revision_foreign_origins_and_maximum_period_payload_fail_without_publication() {
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
    for case in ["future", "management", "foreign-target", "large"] {
        let mut delivery = fixtures::delivery(case, if case == "large" { 1024 } else { 2 });
        let result = fixtures::result_mut(&mut delivery);
        match case {
            "future" => {
                result.schema_version = ContractVersion {
                    major: 1,
                    revision: ContractVersion::V1_DIAGNOSTICS_16.revision + 1,
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
            _ => unreachable!(),
        }
        target.host.try_deliver(delivery).unwrap();
        let completed = report(&mut target).await;
        assert_eq!(completed.delivery_id.as_str(), case);
        assert_eq!(
            completed.outcome,
            DeliveryOutcome::PermanentFailure {
                reason: if case == "large" {
                    "mqtt.payload_too_large"
                } else {
                    "mqtt.canonical_identity_mismatch"
                }
                .to_owned(),
            }
        );
        quiet(&mut peer).await;
    }
    target
        .host
        .try_deliver(fixtures::delivery("after-denials", 2))
        .unwrap();
    let valid = peer.next_publish().await;
    fixtures::assert_payload(&valid.payload, 2);
    peer.acknowledge(&valid).await;
    assert_eq!(
        report(&mut target).await.delivery_id.as_str(),
        "after-denials"
    );
    shutdown(&mut target, &mut peer).await;
}

async fn report(target: &mut RunningTarget) -> DeliveryReport {
    tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .unwrap()
        .unwrap()
}
async fn online(peer: &mut BrokerConnection) {
    let online = peer.next_publish().await;
    assert_eq!(online.topic, "uob/v1/demo/bridge-a/availability");
    peer.acknowledge(&online).await;
}
async fn quiet(peer: &mut BrokerConnection) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err()
    );
}
async fn shutdown(target: &mut RunningTarget, peer: &mut BrokerConnection) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    assert_eq!(offline.topic, "uob/v1/demo/bridge-a/availability");
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
