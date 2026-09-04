mod support;

use std::{sync::Arc, time::Duration};

use serde_json::Value;
use uob_application::{
    BridgeTargetFactory, ConfigurationValue, DeliveryOutcome, TargetConfiguration, TargetMessage,
};
use uob_contracts::{
    BridgeId, DataPointValue, Environment, ExactDecimal, Freshness, PointId, Quality, QualityLevel,
    TargetInstanceId, TypedValue,
};
use uob_mqtt_target_adapter::{MqttRuntimeOptions, MqttTargetFactory};

use support::{
    BrokerConnection, Publication, TestBroker,
    fixtures::{snapshot_delivery, timestamp},
    harness::{RunningTarget, standard_capacities, start_target_with_discovery},
};

#[test]
fn staging_discovery_defaults_off_and_is_advertised_only_when_enabled() {
    let bridge = BridgeId::new("bridge-a").expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Staging).expect("factory");
    let configuration =
        TargetConfiguration::new(TargetInstanceId::new("main").expect("target identity"), 1)
            .with_setting(
                "broker_url",
                ConfigurationValue::Text("mqtts://broker.example:8883".to_owned()),
            );

    let default = create(&factory, &configuration);
    assert!(
        default
            .descriptor()
            .optional_capabilities
            .iter()
            .all(|capability| capability.0 != "home-assistant-discovery")
    );
    let enabled_configuration = configuration.with_setting(
        "home_assistant_discovery",
        ConfigurationValue::Boolean(true),
    );
    let enabled = create(&factory, &enabled_configuration);
    assert!(
        enabled
            .descriptor()
            .optional_capabilities
            .iter()
            .any(|capability| capability.0 == "home-assistant-discovery")
    );
}

fn create(
    factory: &MqttTargetFactory,
    configuration: &TargetConfiguration,
) -> Box<dyn uob_application::BridgeTarget<serde_json::Value, ()>> {
    let validated = <MqttTargetFactory as BridgeTargetFactory<serde_json::Value, ()>>::validate(
        factory,
        configuration,
    )
    .expect("valid MQTT configuration");
    <MqttTargetFactory as BridgeTargetFactory<serde_json::Value, ()>>::create(factory, validated)
        .expect("MQTT target")
}

#[tokio::test]
async fn retained_discovery_and_current_state_recover_after_broker_state_loss() {
    let broker = TestBroker::bind().await;
    let mut target = start_target_with_discovery(
        &broker,
        "bridge/one",
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut first = broker.accept(false).await;
    acknowledge_online(&mut first).await;

    let mut delivery = snapshot_delivery("bridge/one", "station+#", "snapshot-ha");
    let TargetMessage::StationSnapshot(snapshot) = Arc::make_mut(&mut delivery.message) else {
        unreachable!("snapshot fixture")
    };
    snapshot.current_values.push(DataPointValue {
        point_id: PointId::new("power/active.total").expect("point identity"),
        value: Some(TypedValue::Decimal(
            "7200.5".parse::<ExactDecimal>().expect("exact decimal"),
        )),
        source_time: Some(timestamp()),
        observed_at: timestamp(),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    });
    target
        .host
        .try_deliver(delivery)
        .expect("snapshot delivery");

    let state = first.next_publish().await;
    assert_publish(&state, "uob/v1/demo/bridge%2Fone/state/station%2B%23", true);
    first.acknowledge(&state).await;
    assert!(matches!(
        next_report(&mut target).await,
        DeliveryOutcome::Acknowledged { .. }
    ));

    let connectivity = first.next_publish().await;
    assert_publish(
        &connectivity,
        "homeassistant/binary_sensor/uob_demo_6272696467652f6f6e65/station_73746174696f6e2b23_connectivity/config",
        true,
    );
    assert_discovery_payload(&connectivity, "connectivity");
    first.acknowledge(&connectivity).await;

    let telemetry = first.next_publish().await;
    assert_publish(
        &telemetry,
        "homeassistant/sensor/uob_demo_6272696467652f6f6e65/station_73746174696f6e2b23_point_706f7765722f6163746976652e746f74616c/config",
        true,
    );
    let telemetry_payload = json(&telemetry);
    assert_eq!(telemetry_payload["name"], "power/active.total");
    assert!(
        telemetry_payload["value_template"]
            .as_str()
            .expect("value template")
            .contains("power/active.total")
    );
    assert_discovery_payload(&telemetry, "point:706f7765722f6163746976652e746f74616c");
    first.acknowledge(&telemetry).await;

    drop(first);
    let mut recovered = broker.accept(false).await;
    let expected = [
        "uob/v1/demo/bridge%2Fone/availability",
        connectivity.topic.as_str(),
        telemetry.topic.as_str(),
        state.topic.as_str(),
    ];
    for topic in expected {
        let publication = recovered.next_publish().await;
        assert_publish(&publication, topic, true);
        recovered.acknowledge(&publication).await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err(),
        "discovery and state recovery must not create duplicate host reports"
    );

    graceful_shutdown(&mut target, &mut recovered).await;
}

#[tokio::test]
async fn discovery_cache_bound_rejects_a_snapshot_without_partial_entities() {
    let broker = TestBroker::bind().await;
    let runtime = MqttRuntimeOptions {
        discovery_capacity: 1,
        ..MqttRuntimeOptions::default()
    };
    let mut target =
        start_target_with_discovery(&broker, "bridge-a", runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    let mut delivery = snapshot_delivery("bridge-a", "station-a", "bounded-discovery");
    let TargetMessage::StationSnapshot(snapshot) = Arc::make_mut(&mut delivery.message) else {
        unreachable!("snapshot fixture")
    };
    snapshot.current_values.push(DataPointValue {
        point_id: PointId::new("power").expect("point identity"),
        value: Some(TypedValue::SignedInteger(7_200)),
        source_time: None,
        observed_at: timestamp(),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Unknown,
        measurement: None,
    });
    target
        .host
        .try_deliver(delivery)
        .expect("snapshot delivery");
    assert_eq!(
        next_report(&mut target).await,
        DeliveryOutcome::RetryableFailure {
            reason: "mqtt.discovery_capacity".to_owned(),
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err(),
        "an over-capacity discovery set must not partially publish"
    );

    graceful_shutdown(&mut target, &mut peer).await;
}

fn assert_discovery_payload(publication: &Publication, identity_suffix: &str) {
    let payload = json(publication);
    assert_eq!(
        payload["state_topic"],
        "uob/v1/demo/bridge%2Fone/state/station%2B%23"
    );
    assert_eq!(
        payload["availability_topic"],
        "uob/v1/demo/bridge%2Fone/availability"
    );
    assert_eq!(payload["availability_template"], "{{ value_json.status }}");
    assert_eq!(payload["payload_available"], "online");
    assert_eq!(payload["payload_not_available"], "offline");
    assert_eq!(payload["qos"], 1);
    assert!(
        payload["unique_id"]
            .as_str()
            .expect("unique identity")
            .starts_with("uob:demo:6272696467652f6f6e65:73746174696f6e2b23:")
    );
    assert!(
        payload["unique_id"]
            .as_str()
            .expect("unique identity")
            .ends_with(identity_suffix)
    );
    let encoded = String::from_utf8_lossy(&publication.payload).to_ascii_lowercase();
    for secret_name in ["password", "credential", "authorization", "token"] {
        assert!(
            !encoded.contains(secret_name),
            "discovery leaked {secret_name}"
        );
    }
}

async fn acknowledge_online(peer: &mut BrokerConnection) {
    let online = peer.next_publish().await;
    assert!(online.topic.ends_with("/availability"));
    assert_eq!(json(&online)["status"], "online");
    peer.acknowledge(&online).await;
}

async fn next_report(target: &mut RunningTarget) -> DeliveryOutcome {
    tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("delivery report timeout")
        .expect("delivery report channel")
        .outcome
}

async fn graceful_shutdown(target: &mut RunningTarget, peer: &mut BrokerConnection) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    assert!(offline.topic.ends_with("/availability"));
    assert_eq!(json(&offline)["status"], "offline");
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    let result = tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .expect("target shutdown timeout")
        .expect("target task join");
    assert!(result.is_ok(), "target shutdown failed: {result:?}");
}

fn assert_publish(publication: &Publication, topic: &str, retained: bool) {
    assert_eq!(publication.topic, topic);
    assert_eq!(publication.qos, 1);
    assert_eq!(publication.retain, retained);
}

fn json(publication: &Publication) -> Value {
    serde_json::from_slice(&publication.payload).expect("JSON publication")
}
