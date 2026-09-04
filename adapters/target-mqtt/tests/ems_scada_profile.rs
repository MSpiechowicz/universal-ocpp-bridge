mod support;

use std::time::Duration;

use jsonschema::Validator;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uob_application::{
    BridgeTargetFactory, ConfigurationValue, DeliveryOutcome, DeliverySemantic, TargetConfiguration,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandLifecycle, CommandResult, CommandReturnRoute,
    ContractVersion, DataPointDescriptor, DataPointValue, Environment, EventEnvelope,
    TargetInstanceId, UtcTimestamp,
};
use uob_mqtt_target_adapter::{EMS_SCADA_PROFILE, MqttRuntimeOptions, MqttTargetFactory};

use support::{
    BrokerConnection, Publication, TestBroker,
    fixtures::{
        TestEvent, catalog_point_id, event_delivery, point_catalog_snapshot_delivery,
        point_descriptor, point_value, resource, timestamp,
    },
    harness::{RunningTarget, standard_capacities, start_ems_scada_target},
};

const DESCRIPTOR_SCHEMA: &str =
    include_str!("../../../crates/contracts/schemas/v1.0/data-point-descriptor.schema.json");
const VALUE_SCHEMA: &str =
    include_str!("../../../crates/contracts/schemas/v1.0/data-point-value.schema.json");
const EVENT_SCHEMA: &str =
    include_str!("../../../crates/contracts/schemas/v1.0/event-envelope.schema.json");
const COMMAND_SCHEMA: &str =
    include_str!("../../../crates/contracts/schemas/v1.0/command.schema.json");
const COMMAND_RESULT_SCHEMA: &str =
    include_str!("../../../crates/contracts/schemas/v1.0/command-result.schema.json");

const BRIDGE: &str = "site/01";
const STATION: &str = "station-7";
const BASE: &str = "uob/v1/demo/site%2F01";
const ENCODED_POINT: &str = "ocpp16%2Fconnector-1%2FEnergy.Active.Import.Register%2Fall%2FOutlet";

#[test]
fn the_preset_stays_one_target_and_advertises_local_exposure() {
    let bridge = uob_contracts::BridgeId::new(BRIDGE).expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Staging).expect("factory");
    let configuration =
        TargetConfiguration::new(TargetInstanceId::new("main").expect("target identity"), 1)
            .with_setting(
                "broker_url",
                ConfigurationValue::Text("mqtts://broker.example:8883".to_owned()),
            );

    let standard = create(&factory, &configuration).descriptor();
    assert!(
        standard
            .optional_capabilities
            .iter()
            .all(|capability| capability.0 != "ems-scada-point-catalog")
    );
    assert!(
        !standard
            .delivery_semantics
            .contains(&DeliverySemantic::LocalExposure)
    );

    let ems = create(
        &factory,
        &configuration.with_setting(
            "profile",
            ConfigurationValue::Text(EMS_SCADA_PROFILE.to_owned()),
        ),
    )
    .descriptor();
    assert_eq!(
        ems.kind.as_str(),
        standard.kind.as_str(),
        "a preset must not introduce a second target kind or listener"
    );
    assert_eq!(ems.instance_id, standard.instance_id);
    assert!(
        ems.optional_capabilities
            .iter()
            .any(|capability| capability.0 == "ems-scada-point-catalog")
    );
    assert!(
        ems.optional_capabilities
            .iter()
            .all(|capability| capability.0 != "home-assistant-discovery"),
        "the industrial preset must not enable consumer discovery"
    );
    assert!(
        ems.delivery_semantics
            .contains(&DeliverySemantic::LocalExposure),
        "broker exposure must never be reported as EMS-client consumption"
    );
    assert!(
        ems.delivery_semantics
            .contains(&DeliverySemantic::NamedPeerAcknowledgement),
        "broker acknowledgements stay a separate, named delivery scope"
    );
}

#[tokio::test]
async fn retained_point_catalog_matches_the_canonical_contract_schemas() {
    let broker = TestBroker::bind().await;
    let mut target = start_ems_scada_target(
        &broker,
        BRIDGE,
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    target
        .host
        .try_deliver(point_catalog_snapshot_delivery(BRIDGE, STATION, "catalog"))
        .expect("snapshot delivery");

    let state = peer.next_publish().await;
    assert_publish(&state, &format!("{BASE}/state/{STATION}"), true);
    peer.acknowledge(&state).await;
    assert!(matches!(
        next_report(&mut target).await,
        DeliveryOutcome::Acknowledged { .. }
    ));

    let descriptor = peer.next_publish().await;
    assert_publish(
        &descriptor,
        &format!("{BASE}/points/{STATION}/{ENCODED_POINT}"),
        true,
    );
    validate(DESCRIPTOR_SCHEMA, &descriptor);
    let published: DataPointDescriptor =
        serde_json::from_slice(&descriptor.payload).expect("canonical descriptor");
    let connector = published.resource.clone();
    assert_eq!(published, point_descriptor(connector));
    assert_eq!(published.point_id, catalog_point_id());
    peer.acknowledge(&descriptor).await;

    let value = peer.next_publish().await;
    assert_publish(
        &value,
        &format!("{BASE}/values/{STATION}/{ENCODED_POINT}"),
        true,
    );
    validate(VALUE_SCHEMA, &value);
    assert_exact_value(&value);
    peer.acknowledge(&value).await;

    graceful_shutdown(&mut target, &mut peer).await;
}

/// Units, quality, freshness, and both timestamps must survive the wire unchanged.
fn assert_exact_value(publication: &Publication) {
    let published: DataPointValue =
        serde_json::from_slice(&publication.payload).expect("canonical value");
    assert_eq!(published, point_value());

    let document = json(publication);
    assert_eq!(
        document["value"],
        json!({ "type": "decimal", "value": "1234.5" })
    );
    assert_eq!(
        document["measurement"]["original_value"], "1234.500",
        "the exact original text stays beside the normalized quantity"
    );
    assert_eq!(document["measurement"]["original_unit"], "Wh");
    assert_eq!(
        document["quality"],
        json!({ "level": "uncertain", "reason": "signed_data_unverified" })
    );
    assert_eq!(document["source_time"], "1970-01-01T00:01:30Z");
    assert_eq!(document["observed_at"], "1970-01-01T00:00:00Z");
}

#[tokio::test]
async fn canonical_events_and_the_retained_catalog_survive_broker_state_loss() {
    let broker = TestBroker::bind().await;
    let mut target = start_ems_scada_target(
        &broker,
        BRIDGE,
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    target
        .host
        .try_deliver(point_catalog_snapshot_delivery(BRIDGE, STATION, "catalog"))
        .expect("snapshot delivery");
    for _ in 0..3 {
        let publication = peer.next_publish().await;
        peer.acknowledge(&publication).await;
    }
    assert!(matches!(
        next_report(&mut target).await,
        DeliveryOutcome::Acknowledged { .. }
    ));

    // A durable event keeps its canonical identity and schema under the preset.
    target
        .host
        .try_deliver(event_delivery(
            BRIDGE,
            STATION,
            "catalog-event",
            "event-1",
            Environment::Demo,
        ))
        .expect("event delivery");
    let event = peer.next_publish().await;
    assert_publish(&event, &format!("{BASE}/events/{STATION}/event-1"), false);
    validate(EVENT_SCHEMA, &event);
    let envelope: EventEnvelope<TestEvent> =
        serde_json::from_slice(&event.payload).expect("canonical event");
    assert_eq!(envelope.event_id.as_str(), "event-1");
    assert_eq!(envelope.resource, resource(BRIDGE, STATION));
    peer.acknowledge(&event).await;
    assert!(matches!(
        next_report(&mut target).await,
        DeliveryOutcome::Acknowledged { .. }
    ));

    drop(peer);
    let mut recovered = broker.accept(false).await;
    for topic in [
        format!("{BASE}/availability"),
        format!("{BASE}/points/{STATION}/{ENCODED_POINT}"),
        format!("{BASE}/values/{STATION}/{ENCODED_POINT}"),
        format!("{BASE}/state/{STATION}"),
    ] {
        let publication = recovered.next_publish().await;
        assert_publish(&publication, &topic, true);
        recovered.acknowledge(&publication).await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err(),
        "retained catalog recovery must not create duplicate host delivery reports"
    );

    graceful_shutdown(&mut target, &mut recovered).await;
}

#[tokio::test]
async fn authorized_commands_stay_unretained_and_correlated_under_the_preset() {
    let broker = TestBroker::bind().await;
    let mut target = start_ems_scada_target(
        &broker,
        BRIDGE,
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;
    if peer.subscriptions().is_empty() {
        peer.next_subscription().await;
    }
    assert_eq!(
        peer.subscriptions(),
        &[format!("{BASE}/commands/+/+")],
        "the preset reuses exactly one authorized command namespace"
    );

    let request = json!({
        "schema_version": { "major": 1, "revision": 0 },
        "request_id": "ems-1",
        "correlation_id": "ems-correlation-1",
        "resource": { "bridge_id": BRIDGE, "station_id": STATION },
        "operation": {
            "kind": "set_charging_limit",
            "parameters": { "value": "7200", "unit": "watt" }
        },
        "expires_at": UtcTimestamp::new(OffsetDateTime::now_utc() + Duration::from_secs(60))
    });
    // The broker payload carries no trusted origin or admission instant; adding exactly the fields
    // the host supplies must produce the same canonical command a direct HTTP client submits.
    let mut admitted_document = request.clone();
    admitted_document["origin"] = json!({
        "kind": "target",
        "target_instance_id": "main",
        "principal_id": "mqtt-target:main"
    });
    admitted_document["admitted_at"] = json!(timestamp());
    validate_value(COMMAND_SCHEMA, &admitted_document);
    peer.publish_command(
        &format!("{BASE}/commands/{STATION}/ems-1"),
        &serde_json::to_vec(&request).expect("command payload"),
        21,
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
        resource(BRIDGE, STATION)
    );
    assert!(matches!(
        &submission.command.origin,
        AuthenticatedCommandOrigin::Target { principal_id, .. }
            if principal_id.as_str() == "mqtt-target:main"
    ));
    let result = CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: submission.command.request.correlation_id.clone(),
        resource: submission.command.request.resource.clone(),
        return_route: CommandReturnRoute {
            request_id: submission.command.request.request_id.clone(),
            origin: submission.command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Admitted,
        recorded_at: timestamp(),
        observed_effects: vec![],
    };
    submission
        .respond(Ok(result.clone()))
        .expect("host response");

    let published = peer.next_publish().await;
    assert_publish(
        &published,
        &format!("{BASE}/results/{STATION}/ems-1"),
        false,
    );
    validate(COMMAND_RESULT_SCHEMA, &published);
    assert_eq!(
        serde_json::from_slice::<CommandResult>(&published.payload).expect("canonical result"),
        result
    );
    assert_eq!(json(&published)["correlation_id"], "ems-correlation-1");
    peer.acknowledge(&published).await;

    graceful_shutdown(&mut target, &mut peer).await;
}

#[tokio::test]
async fn point_catalog_bound_rejects_a_snapshot_without_partial_publication() {
    let broker = TestBroker::bind().await;
    let runtime = MqttRuntimeOptions {
        point_catalog_capacity: 1,
        ..MqttRuntimeOptions::default()
    };
    let mut target = start_ems_scada_target(&broker, BRIDGE, runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer).await;

    target
        .host
        .try_deliver(point_catalog_snapshot_delivery(BRIDGE, STATION, "bounded"))
        .expect("snapshot delivery");

    assert_eq!(
        next_report(&mut target).await,
        DeliveryOutcome::RetryableFailure {
            reason: "mqtt.point_catalog_capacity".to_owned(),
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next_publish())
            .await
            .is_err(),
        "an over-capacity point catalog must not partially publish"
    );

    graceful_shutdown(&mut target, &mut peer).await;
}

fn create(
    factory: &MqttTargetFactory,
    configuration: &TargetConfiguration,
) -> Box<dyn uob_application::BridgeTarget<TestEvent, ()>> {
    let validated =
        <MqttTargetFactory as BridgeTargetFactory<TestEvent, ()>>::validate(factory, configuration)
            .expect("valid MQTT configuration");
    <MqttTargetFactory as BridgeTargetFactory<TestEvent, ()>>::create(factory, validated)
        .expect("MQTT target")
}

fn validate(schema: &str, publication: &Publication) {
    validate_value(schema, &json(publication));
}

fn validate_value(schema: &str, document: &Value) {
    let schema: Value = serde_json::from_str(schema).expect("published contract schema");
    let validator = Validator::new(&schema).expect("compiled contract schema");
    let errors = validator
        .iter_errors(document)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "published payload does not match the direct HTTP contract schema: {errors:?}"
    );
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
