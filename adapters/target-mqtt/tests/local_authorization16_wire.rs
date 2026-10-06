//! Consume test-host native ACKs without enabling privileged MQTT command production.
#[path = "local_authorization16_wire/fixtures.rs"]
mod fixtures;
#[path = "local_authorization16_wire/immediate.rs"]
mod immediate;
mod support;
use serde_json::{Value, json};
use std::time::Duration;
use support::{
    BrokerConnection, Publication, TestBroker,
    harness::{RunningTarget, standard_capacities, start_target},
};
use uob_application::{DeliveryOutcome, DeliveryReport};
use uob_contracts::{ContractVersion, EventId};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

#[tokio::test]
async fn revision_nine_native_evidence_and_ordinary_results_use_durable_broker_receipts() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    online(&mut peer).await;
    for (id, evidence) in fixtures::cases() {
        target
            .host
            .try_deliver(fixtures::delivery(id, evidence.clone()))
            .unwrap();
        let publication = peer.next_publish().await;
        assert_publication(&publication, id, evidence.as_ref());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
                .await
                .is_err()
        );
        peer.acknowledge(&publication).await;
        let completed = report(&mut target).await;
        assert_eq!(completed.delivery_id.as_str(), id);
        assert_broker_receipt(completed.outcome);
    }
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn unsupported_revisions_and_oversized_native_results_fail_without_broker_publication() {
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
    let evidence = json!({"action":"ClearCache","status":"Accepted"});
    for case in ["revision-2", "revision-3", "future", "large", "valid"] {
        let mut delivery = fixtures::delivery(case, Some(evidence.clone()));
        let result = fixtures::result_mut(&mut delivery);
        alter(result, case);
        target.host.try_deliver(delivery).unwrap();
        if case == "valid" {
            let publication = peer.next_publish().await;
            assert!(publication.payload.len() <= 4096);
            assert_publication(&publication, case, Some(&evidence));
            peer.acknowledge(&publication).await;
            assert_broker_receipt(report(&mut target).await.outcome);
        } else {
            let completed = report(&mut target).await;
            assert_eq!(completed.delivery_id.as_str(), case);
            assert_eq!(
                completed.outcome,
                DeliveryOutcome::PermanentFailure {
                    reason: rejection(case).to_owned()
                }
            );
            quiet(&mut peer).await;
        }
    }
    shutdown(&mut target, &mut peer).await;
}
fn alter(result: &mut uob_contracts::CommandResult, case: &str) {
    match case {
        "revision-2" => {
            result.schema_version = ContractVersion {
                major: 1,
                revision: 2,
            }
        }
        "revision-3" => {
            result.schema_version = ContractVersion {
                major: 1,
                revision: 3,
            }
        }
        "future" => {
            result.schema_version = ContractVersion {
                major: 1,
                revision: ContractVersion::V1_FIRMWARE_16.revision + 1,
            }
        }
        "large" => {
            result.observed_effects = (0..400)
                .map(|index| uob_contracts::ObservedCommandEffect {
                    event_id: EventId::new(format!("safe-effect-{index}")).unwrap(),
                    event_type: uob_contracts::EventType::new("meter.changed.v1").unwrap(),
                    observed_at: result.recorded_at,
                })
                .collect();
            assert!(serde_json::to_vec(result).unwrap().len() > 4096);
        }
        "valid" => {}
        _ => unreachable!(),
    }
}
fn rejection(case: &str) -> &'static str {
    if case == "large" {
        "mqtt.payload_too_large"
    } else {
        "mqtt.canonical_identity_mismatch"
    }
}
fn assert_publication(publication: &Publication, request: &str, evidence: Option<&Value>) {
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
