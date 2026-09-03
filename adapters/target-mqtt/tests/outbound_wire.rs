mod support;

use std::time::Duration;

use serde_json::Value;
use uob_application::{DeliveryOutcome, TargetMessage};
use uob_contracts::{CommandResult, Environment, EventEnvelope, StationSnapshot, TraceRecord};
use uob_mqtt_target_adapter::MqttRuntimeOptions;

use support::{
    Publication, TestBroker,
    fixtures::{TestEvent, event_delivery, result_delivery, snapshot_delivery, trace_delivery},
    harness::{RunningTarget, standard_capacities, start_target},
};

#[tokio::test]
async fn publishes_canonical_outputs_with_versioned_topics_and_broker_ack_truth() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "trusted/bridge+#",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;

    assert_eq!(peer.connect().protocol_level, 4, "MQTT 3.1.1/V4");
    assert!(!peer.connect().clean_session);
    assert_eq!(
        peer.connect().client_id,
        "uob-v1-demo-trusted%2Fbridge%2B%23-main"
    );
    let will = peer.connect().will.as_ref().expect("offline last will");
    assert_eq!(
        will.topic,
        "uob/v1/demo/trusted%2Fbridge%2B%23/availability"
    );
    assert_eq!(will.qos, 1);
    assert!(will.retain);
    assert!(will.packet_id.is_none());
    assert_eq!(json(will)["status"], "offline");

    let online = peer.next_publish().await;
    assert_publication(
        &online,
        "uob/v1/demo/trusted%2Fbridge%2B%23/availability",
        true,
    );
    assert_eq!(json(&online)["status"], "online");
    assert_eq!(json(&online)["environment"], "demo");
    assert_eq!(json(&online)["bridge_id"], "trusted/bridge+#");
    peer.acknowledge(&online).await;

    assert_canonical_deliveries(&mut target, &mut peer).await;

    assert!(
        tokio::time::timeout(Duration::from_millis(50), target.host.next_command())
            .await
            .is_err(),
        "outbound MQTT must not implement command ingress"
    );
    graceful_shutdown(
        &mut target,
        &mut peer,
        "uob/v1/demo/trusted%2Fbridge%2B%23/availability",
    )
    .await;
}

#[tokio::test]
async fn payload_environment_cannot_spoof_the_trusted_factory_namespace() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(
        &broker,
        "trusted/bridge+#",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    let online = peer.next_publish().await;
    peer.acknowledge(&online).await;

    target
        .host
        .try_deliver(event_delivery(
            "trusted/bridge+#",
            "station-a",
            "spoofed-environment",
            "event-a",
            Environment::Production,
        ))
        .expect("delivery");
    let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("mapping report timeout")
        .expect("mapping report channel");
    assert_eq!(
        report.outcome,
        DeliveryOutcome::PermanentFailure {
            reason: "mqtt.canonical_identity_mismatch".to_owned(),
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err(),
        "spoofed payload must not reach any MQTT namespace"
    );
    graceful_shutdown(
        &mut target,
        &mut peer,
        "uob/v1/demo/trusted%2Fbridge%2B%23/availability",
    )
    .await;
}

#[tokio::test]
async fn outgoing_packet_bound_includes_a_large_valid_topic() {
    let broker = TestBroker::bind().await;
    let station = "s".repeat(50_000);
    let delivery = snapshot_delivery("bridge-a", &station, "large-topic");
    let payload_bytes = match delivery.message.as_ref() {
        TargetMessage::StationSnapshot(snapshot) => serde_json::to_vec(snapshot)
            .expect("canonical snapshot")
            .len(),
        _ => unreachable!(),
    };
    let runtime = MqttRuntimeOptions {
        maximum_message_bytes: payload_bytes,
        ..MqttRuntimeOptions::default()
    };
    let mut target = start_target(&broker, "bridge-a", runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    let online = peer.next_publish().await;
    peer.acknowledge(&online).await;

    target
        .host
        .try_deliver(delivery)
        .expect("large-topic delivery");
    let publication = peer.next_publish().await;
    assert_eq!(
        publication.topic,
        format!("uob/v1/demo/bridge-a/state/{station}")
    );
    assert_eq!(publication.payload.len(), payload_bytes);
    assert!(publication.topic.len() > 4096);
    peer.acknowledge(&publication).await;
    let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("large-topic report timeout")
        .expect("large-topic report channel");
    assert!(matches!(
        report.outcome,
        DeliveryOutcome::Acknowledged { .. }
    ));
    graceful_shutdown(&mut target, &mut peer, "uob/v1/demo/bridge-a/availability").await;
}

#[tokio::test]
async fn oversized_json_stops_at_the_payload_bound_without_publishing() {
    let broker = TestBroker::bind().await;
    let runtime = MqttRuntimeOptions {
        maximum_message_bytes: 512,
        ..MqttRuntimeOptions::default()
    };
    let mut target = start_target(&broker, "bridge-a", runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    let online = peer.next_publish().await;
    peer.acknowledge(&online).await;

    target
        .host
        .try_deliver(snapshot_delivery(
            "bridge-a",
            &"s".repeat(10_000),
            "oversized-json",
        ))
        .expect("oversized delivery");
    let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("oversized report timeout")
        .expect("oversized report channel");
    assert_eq!(
        report.outcome,
        DeliveryOutcome::PermanentFailure {
            reason: "mqtt.payload_too_large".to_owned(),
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err(),
        "oversized canonical JSON must not be queued to rumqttc"
    );
    graceful_shutdown(&mut target, &mut peer, "uob/v1/demo/bridge-a/availability").await;
}

async fn assert_canonical_deliveries(
    target: &mut RunningTarget,
    peer: &mut support::BrokerConnection,
) {
    let deliveries = [
        snapshot_delivery("trusted/bridge+#", "station/+#", "snapshot-1"),
        event_delivery(
            "trusted/bridge+#",
            "station/+#",
            "event-delivery-1",
            "event/#",
            Environment::Demo,
        ),
        result_delivery(
            "trusted/bridge+#",
            "station/+#",
            "result-delivery-1",
            "request/+",
        ),
        trace_delivery(
            "trusted/bridge+#",
            "station/+#",
            "trace-delivery-1",
            "trace/%",
        ),
    ];
    let expected = [
        (
            "uob/v1/demo/trusted%2Fbridge%2B%23/state/station%2F%2B%23",
            true,
        ),
        (
            "uob/v1/demo/trusted%2Fbridge%2B%23/events/station%2F%2B%23/event%2F%23",
            false,
        ),
        (
            "uob/v1/demo/trusted%2Fbridge%2B%23/results/station%2F%2B%23/request%2F%2B",
            false,
        ),
        (
            "uob/v1/demo/trusted%2Fbridge%2B%23/traces/station%2F%2B%23/trace%2F%25",
            false,
        ),
    ];

    for (index, (delivery, (topic, retained))) in deliveries.into_iter().zip(expected).enumerate() {
        target.host.try_deliver(delivery).expect("bounded delivery");
        let publication = peer.next_publish().await;
        assert_publication(&publication, topic, retained);
        match index {
            0 => {
                serde_json::from_slice::<StationSnapshot>(&publication.payload)
                    .expect("snapshot schema");
            }
            1 => {
                serde_json::from_slice::<EventEnvelope<TestEvent>>(&publication.payload)
                    .expect("event schema");
            }
            2 => {
                serde_json::from_slice::<CommandResult>(&publication.payload)
                    .expect("command-result schema");
            }
            3 => {
                serde_json::from_slice::<TraceRecord>(&publication.payload).expect("trace schema");
            }
            _ => unreachable!(),
        }
        assert_eq!(json(&publication)["schema_version"]["major"], 1);
        peer.acknowledge(&publication).await;
        let report = tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
            .await
            .expect("delivery report timeout")
            .expect("delivery report channel");
        match report.outcome {
            DeliveryOutcome::Acknowledged { scope, .. } => {
                assert_eq!(scope.0, "mqtt.broker_received");
            }
            outcome => panic!("PUBACK must report broker receipt, got {outcome:?}"),
        }
    }
}

fn assert_publication(publication: &Publication, topic: &str, retain: bool) {
    assert_eq!(publication.topic, topic);
    assert_eq!(publication.qos, 1);
    assert_eq!(publication.retain, retain);
    assert!(!publication.duplicate);
    assert!(publication.packet_id.is_some());
}

fn json(publication: &Publication) -> Value {
    serde_json::from_slice(&publication.payload).expect("JSON publication")
}

async fn graceful_shutdown(
    target: &mut RunningTarget,
    peer: &mut support::BrokerConnection,
    availability_topic: &str,
) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    assert_publication(&offline, availability_topic, true);
    assert_eq!(json(&offline)["status"], "offline");
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    let result = tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .expect("target shutdown timeout")
        .expect("target task join");
    assert!(result.is_ok(), "target shutdown failed: {result:?}");
}
