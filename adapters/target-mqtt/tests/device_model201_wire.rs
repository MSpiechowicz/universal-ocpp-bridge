#[path = "device_model201_wire/fixtures.rs"]
mod fixtures;
mod support;

use std::time::Duration;

use serde_json::{Value, json};
use uob_application::{DeliveryOutcome, DeliveryReport, TargetMessage};
use uob_contracts::{AuthenticatedCommandOrigin, BridgeId, ContractVersion, TargetInstanceId};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

use fixtures::{device_delivery, result_mut, rich_report};
use support::{
    BrokerConnection, Publication, TestBroker,
    harness::{RunningTarget, standard_capacities, start_target},
};

const REPORT_TIMEOUT: Duration = Duration::from_secs(2);
const QUIET_INTERVAL: Duration = Duration::from_millis(100);

#[tokio::test]
async fn accepted_incomplete_device_result_waits_for_its_own_puback() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    // This is authoritative test-only target-origin state, not privileged command ingress.
    target
        .host
        .try_deliver(device_delivery("device-first", "inventory/first"))
        .expect("first device delivery");
    let first = peer.next_publish().await;
    assert_device_publication(&first, "inventory%2Ffirst");

    target
        .host
        .try_deliver(device_delivery("device-second", "inventory/second"))
        .expect("second device delivery");
    let second = peer.next_publish().await;
    assert_device_publication(&second, "inventory%2Fsecond");
    assert_ne!(first.packet_id, second.packet_id);
    assert!(
        tokio::time::timeout(QUIET_INTERVAL, target.host.next_report())
            .await
            .is_err(),
        "writing device results to TCP does not establish broker receipt"
    );

    peer.acknowledge(&second).await;
    let report = next_report(&mut target).await;
    assert_eq!(report.delivery_id.as_str(), "device-second");
    assert_broker_receipt(report.outcome);
    assert!(
        tokio::time::timeout(QUIET_INTERVAL, target.host.next_report())
            .await
            .is_err(),
        "the second result PUBACK must not acknowledge the first result"
    );

    peer.acknowledge(&first).await;
    let report = next_report(&mut target).await;
    assert_eq!(report.delivery_id.as_str(), "device-first");
    assert_broker_receipt(report.outcome);
    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn device_result_origin_instance_bridge_and_future_version_are_fail_closed() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "bridge-a",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    for case in [
        "management-origin",
        "foreign-origin-instance",
        "foreign-delivery-instance",
        "foreign-payload-bridge",
        "foreign-envelope-bridge",
        "future-version",
    ] {
        let mut delivery = device_delivery(case, "inventory/boundary");
        match case {
            "management-origin" => {
                result_mut(&mut delivery).return_route.origin =
                    AuthenticatedCommandOrigin::Management {
                        principal_id: uob_contracts::PrincipalId::new("operator-a").unwrap(),
                    };
            }
            "foreign-origin-instance" => {
                let AuthenticatedCommandOrigin::Target {
                    target_instance_id, ..
                } = &mut result_mut(&mut delivery).return_route.origin
                else {
                    unreachable!("test-only target origin");
                };
                *target_instance_id = TargetInstanceId::new("other").unwrap();
            }
            "foreign-delivery-instance" => {
                delivery.target_instance_id = TargetInstanceId::new("other").unwrap();
            }
            "foreign-payload-bridge" => {
                result_mut(&mut delivery).resource.bridge_id = BridgeId::new("other").unwrap();
            }
            "foreign-envelope-bridge" => {
                delivery.station_ordering_key.bridge_id = BridgeId::new("other").unwrap();
                result_mut(&mut delivery).resource.bridge_id = BridgeId::new("other").unwrap();
            }
            "future-version" => {
                result_mut(&mut delivery).schema_version = ContractVersion {
                    major: 1,
                    revision: 6,
                };
            }
            _ => unreachable!(),
        }
        target
            .host
            .try_deliver(delivery)
            .expect("boundary delivery");
        let report = next_report(&mut target).await;
        assert_eq!(report.delivery_id.as_str(), case);
        assert_eq!(
            report.outcome,
            DeliveryOutcome::PermanentFailure {
                reason: "mqtt.canonical_identity_mismatch".to_owned(),
            },
            "invalid device result must fail before broker handoff: {case}"
        );
        assert_no_publication(&mut peer).await;
    }

    shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn oversized_rich_device_report_is_rejected_without_truncation_or_larger_cap() {
    const MAXIMUM_MESSAGE_BYTES: usize = 4096;
    let broker = TestBroker::bind().await;
    let runtime = MqttRuntimeOptions {
        maximum_message_bytes: MAXIMUM_MESSAGE_BYTES,
        ..MqttRuntimeOptions::default()
    };
    let mut target = start_target(&broker, "bridge-a", runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    let mut oversized = device_delivery("rich-report", "inventory/rich");
    result_mut(&mut oversized)
        .device_model_201
        .as_mut()
        .expect("device evidence")
        .report = rich_report();
    let TargetMessage::CommandResult(result) = oversized.message.as_ref() else {
        unreachable!();
    };
    assert!(
        serde_json::to_vec(result).expect("rich result JSON").len() > MAXIMUM_MESSAGE_BYTES,
        "the report must exercise the payload bound, not an unrelated mapping error"
    );
    target.host.try_deliver(oversized).expect("rich delivery");
    let report = next_report(&mut target).await;
    assert_eq!(report.delivery_id.as_str(), "rich-report");
    assert_eq!(
        report.outcome,
        DeliveryOutcome::PermanentFailure {
            reason: "mqtt.payload_too_large".to_owned(),
        }
    );
    assert_no_publication(&mut peer).await;

    // The same configured cap remains usable; failure neither truncates inventory nor poisons output.
    target
        .host
        .try_deliver(device_delivery("small-after-rich", "inventory/small"))
        .expect("small delivery after oversized result");
    let publication = peer.next_publish().await;
    assert!(publication.payload.len() <= MAXIMUM_MESSAGE_BYTES);
    assert_device_publication(&publication, "inventory%2Fsmall");
    peer.acknowledge(&publication).await;
    let report = next_report(&mut target).await;
    assert_eq!(report.delivery_id.as_str(), "small-after-rich");
    assert_broker_receipt(report.outcome);
    shutdown(&mut target, &mut peer).await;
}

fn assert_device_publication(publication: &Publication, encoded_request: &str) {
    assert_eq!(
        publication.topic,
        format!("uob/v1/demo/bridge-a/results/station-a/{encoded_request}")
    );
    assert_eq!(publication.qos, 1);
    assert!(!publication.retain);
    assert!(!publication.duplicate);
    assert!(publication.packet_id.is_some());

    let payload: Value = serde_json::from_slice(&publication.payload).expect("device result JSON");
    assert_eq!(
        payload["schema_version"],
        json!({"major": 1, "revision": 5})
    );
    assert_eq!(payload["return_route"]["origin"]["kind"], "target");
    assert_eq!(
        payload["return_route"]["origin"]["target_instance_id"],
        "main"
    );
    assert_eq!(payload["lifecycle"]["stage"], "protocol_response");
    assert_eq!(payload["lifecycle"]["accepted"], true);
    let evidence = &payload["device_model_201"];
    assert_eq!(evidence["query"]["action"], "GetBaseReport");
    assert_eq!(evidence["query"]["request_id"], json!(-2_147_483_648_i32));
    assert_eq!(evidence["query"]["report_base"], "FullInventory");
    assert_eq!(evidence["connection"], "native-connection-a");
    assert_eq!(evidence["generation"], 7);
    assert_eq!(evidence["native_ack"], "Accepted");
    assert_eq!(
        evidence["report"],
        json!({
            "state": "incomplete",
            "reason": "timeout",
            "progress": {"fragments": 1, "items": 2, "bytes": 320}
        }),
        "native acceptance and broker receipt do not imply report completion or partial inventory"
    );
}

async fn acknowledge_online(peer: &mut BrokerConnection) {
    let online = peer.next_publish().await;
    assert_eq!(online.topic, "uob/v1/demo/bridge-a/availability");
    peer.acknowledge(&online).await;
}

async fn next_report(target: &mut RunningTarget) -> DeliveryReport {
    tokio::time::timeout(REPORT_TIMEOUT, target.host.next_report())
        .await
        .expect("delivery report timeout")
        .expect("delivery report channel")
}

fn assert_broker_receipt(outcome: DeliveryOutcome) {
    match outcome {
        DeliveryOutcome::Acknowledged { scope, .. } => {
            assert_eq!(scope.0, "mqtt.broker_received");
        }
        other => panic!("PUBACK must establish only broker receipt, got {other:?}"),
    }
}

async fn assert_no_publication(peer: &mut BrokerConnection) {
    assert!(
        tokio::time::timeout(QUIET_INTERVAL, peer.next_publish())
            .await
            .is_err(),
        "invalid or oversized device evidence must not publish, including truncated payloads"
    );
}

async fn shutdown(target: &mut RunningTarget, peer: &mut BrokerConnection) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    assert_eq!(offline.topic, "uob/v1/demo/bridge-a/availability");
    assert_eq!(offline.qos, 1);
    assert!(offline.retain);
    let payload: Value = serde_json::from_slice(&offline.payload).expect("offline JSON");
    assert_eq!(payload["status"], "offline");
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    let result = tokio::time::timeout(REPORT_TIMEOUT, &mut target.task)
        .await
        .expect("target shutdown timeout")
        .expect("target task join");
    assert!(result.is_ok(), "target shutdown failed: {result:?}");
}
