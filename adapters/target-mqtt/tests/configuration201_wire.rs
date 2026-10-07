//! Seeded test-only target-owned native ACKs; no privileged MQTT native producer is enabled.
#[path = "configuration201_wire/fixtures.rs"]
mod fixtures;
#[path = "configuration201_wire/immediate.rs"]
mod immediate;
mod support;

use serde_json::{Value, json};
use std::time::Duration;
use support::{
    BrokerConnection, Publication, TestBroker,
    harness::{RunningTarget, standard_capacities, start_target},
};
use uob_application::{DeliveryOutcome, DeliveryReport};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, ContractVersion, PrincipalId, StationId, TargetInstanceId,
};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

#[tokio::test]
async fn configuration_mixed_items_and_staged_network_wait_for_their_own_durable_puback() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    let mixed = fixtures::variables(6);
    let staged = fixtures::network("Accepted");
    target
        .host
        .try_deliver(fixtures::delivery("mixed/items", mixed.clone()))
        .unwrap();
    let first = peer.next_publish().await;
    assert_publication(&first, "mixed%2Fitems", &mixed);
    assert_eq!(
        serde_json::from_slice::<Value>(&first.payload).unwrap()["lifecycle"]["accepted"],
        false
    );
    target
        .host
        .try_deliver(fixtures::delivery("staged", staged.clone()))
        .unwrap();
    let second = peer.next_publish().await;
    assert_publication(&second, "staged", &staged);
    assert_ne!(first.packet_id, second.packet_id);
    assert_no_report(&mut target).await;
    peer.acknowledge(&second).await;
    let completed = report(&mut target).await;
    assert_eq!(completed.delivery_id.as_str(), "staged");
    assert_broker_receipt(completed.outcome);
    assert_no_report(&mut target).await;
    peer.acknowledge(&first).await;
    let completed = report(&mut target).await;
    assert_eq!(completed.delivery_id.as_str(), "mixed/items");
    assert_broker_receipt(completed.outcome);
    for (id, evidence) in [
        ("Rejected", fixtures::network("Rejected")),
        ("Failed", fixtures::network("Failed")),
        ("RebootRequired", fixtures::reboot_required()),
    ] {
        target
            .host
            .try_deliver(fixtures::delivery(id, evidence.clone()))
            .unwrap();
        let publication = peer.next_publish().await;
        assert_publication(&publication, id, &evidence);
        peer.acknowledge(&publication).await;
        let completed = report(&mut target).await;
        assert_eq!(completed.delivery_id.as_str(), id);
        assert_broker_receipt(completed.outcome);
    }
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn configuration_future_version_and_foreign_identity_fail_before_broker_handoff() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    for case in [
        "future",
        "management",
        "foreign-origin",
        "foreign-delivery",
        "foreign-bridge",
        "foreign-station",
        "foreign-envelope",
    ] {
        let mut delivery = fixtures::delivery(case, fixtures::network("Accepted"));
        match case {
            "future" => {
                fixtures::result_mut(&mut delivery).schema_version = ContractVersion {
                    major: 1,
                    revision: ContractVersion::V1_DIAGNOSTICS_201.revision + 1,
                }
            }
            "management" => {
                fixtures::result_mut(&mut delivery).return_route.origin =
                    AuthenticatedCommandOrigin::Management {
                        principal_id: PrincipalId::new("operator").unwrap(),
                    }
            }
            "foreign-origin" => {
                let AuthenticatedCommandOrigin::Target {
                    target_instance_id, ..
                } = &mut fixtures::result_mut(&mut delivery).return_route.origin
                else {
                    unreachable!()
                };
                *target_instance_id = TargetInstanceId::new("other").unwrap();
            }
            "foreign-delivery" => {
                delivery.target_instance_id = TargetInstanceId::new("other").unwrap();
            }
            "foreign-bridge" => {
                fixtures::result_mut(&mut delivery).resource.bridge_id =
                    BridgeId::new("other").unwrap();
            }
            "foreign-station" => {
                fixtures::result_mut(&mut delivery).resource.station_id =
                    StationId::new("other").unwrap();
            }
            "foreign-envelope" => {
                delivery.station_ordering_key.bridge_id = BridgeId::new("other").unwrap();
            }
            _ => unreachable!(),
        }
        target.host.try_deliver(delivery).unwrap();
        let completed = report(&mut target).await;
        assert_eq!(completed.delivery_id.as_str(), case);
        assert_eq!(
            completed.outcome,
            DeliveryOutcome::PermanentFailure {
                reason: "mqtt.canonical_identity_mismatch".to_owned()
            }
        );
        quiet(&mut peer).await;
    }
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn configuration_payload_cap_rejects_complete_large_result_without_truncation() {
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
    let mut large = fixtures::delivery("large", fixtures::variables(128));
    assert!(
        serde_json::to_vec(fixtures::result_mut(&mut large))
            .unwrap()
            .len()
            > 4096
    );
    target.host.try_deliver(large).unwrap();
    let completed = report(&mut target).await;
    assert_eq!(completed.delivery_id.as_str(), "large");
    assert_eq!(
        completed.outcome,
        DeliveryOutcome::PermanentFailure {
            reason: "mqtt.payload_too_large".to_owned()
        }
    );
    quiet(&mut peer).await;
    let small = fixtures::network("Accepted");
    target
        .host
        .try_deliver(fixtures::delivery("small", small.clone()))
        .unwrap();
    let publication = peer.next_publish().await;
    assert!(publication.payload.len() <= 4096);
    assert_publication(&publication, "small", &small);
    peer.acknowledge(&publication).await;
    assert_broker_receipt(report(&mut target).await.outcome);
    shutdown(&mut target, &mut peer).await;
}

fn assert_publication(publication: &Publication, request: &str, evidence: &Value) {
    assert_eq!(
        publication.topic,
        format!("uob/v1/demo/bridge-a/results/station-a/{request}")
    );
    assert_eq!(publication.qos, 1);
    assert!(!publication.retain);
    assert!(!publication.duplicate);
    assert!(publication.packet_id.is_some());
    fixtures::assert_payload(&publication.payload, evidence);
}

async fn report(target: &mut RunningTarget) -> DeliveryReport {
    tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .unwrap()
        .unwrap()
}

fn assert_broker_receipt(outcome: DeliveryOutcome) {
    assert!(
        matches!(outcome, DeliveryOutcome::Acknowledged { scope, .. } if scope.0 == "mqtt.broker_received")
    );
}

async fn assert_no_report(target: &mut RunningTarget) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err()
    );
}

async fn online(peer: &mut BrokerConnection) {
    let publication = peer.next_publish().await;
    assert_eq!(publication.topic, "uob/v1/demo/bridge-a/availability");
    peer.acknowledge(&publication).await;
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
    assert_eq!(
        serde_json::from_slice::<Value>(&offline.payload).unwrap()["status"],
        json!("offline")
    );
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
