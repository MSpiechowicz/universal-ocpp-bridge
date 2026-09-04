use std::{sync::Arc, time::Duration};

use rumqttc::{ConnAck, ConnectReturnCode, Event, Incoming, Outgoing, PubAck};
use time::OffsetDateTime;
use uob_application::{DeliveryId, DeliveryOutcome, TargetRuntimeLimits};
use uob_contracts::{BridgeId, Environment, TargetInstanceId, UtcTimestamp};
use uob_target_conformance::{FakeTargetHost, HostCapacities, UnsupportedQueryPort};

use super::{PublishPurpose, Session, TrackedPurpose};
use crate::{
    configuration::{BrokerEndpoint, MqttRuntimeOptions, MqttSettings},
    mapping::TopicNamespace,
    target::MqttTarget,
};

#[tokio::test]
async fn buffered_outgoing_then_puback_acknowledges_the_original_delivery() {
    let (mut session, mut host) = test_session();
    session.awaiting_packet_id.push_back(tracked("old", 1));

    session.reconcile_buffered(vec![
        Event::Outgoing(Outgoing::Publish(7)),
        Event::Incoming(Incoming::PubAck(PubAck::new(7))),
    ]);

    assert!(session.awaiting_packet_id.is_empty());
    assert!(session.in_flight.is_empty());
    let report = host.next_report().await.expect("broker ACK report");
    assert_eq!(report.delivery_id.as_str(), "old");
    assert!(matches!(
        report.outcome,
        DeliveryOutcome::Acknowledged { .. }
    ));
}

#[tokio::test]
async fn stale_buffered_outgoing_cannot_consume_a_new_session_purpose() {
    let (mut session, mut host) = test_session();
    session.awaiting_packet_id.push_back(tracked("old", 1));
    session.reconcile_buffered(vec![Event::Outgoing(Outgoing::Publish(1))]);

    session
        .handle_event(Event::Incoming(Incoming::ConnAck(ConnAck::new(
            ConnectReturnCode::Success,
            false,
        ))))
        .expect("fresh session");
    let lost = host.next_report().await.expect("lost-session report");
    assert_eq!(lost.delivery_id.as_str(), "old");
    assert!(matches!(lost.outcome, DeliveryOutcome::Uncertain { .. }));

    session.awaiting_packet_id.push_back(tracked("new", 2));
    session
        .handle_event(Event::Outgoing(Outgoing::Publish(1)))
        .expect("new publish correlation");
    session
        .handle_event(Event::Incoming(Incoming::PubAck(PubAck::new(1))))
        .expect("new broker ACK");
    let acknowledged = host.next_report().await.expect("new-session report");
    assert_eq!(acknowledged.delivery_id.as_str(), "new");
    assert!(matches!(
        acknowledged.outcome,
        DeliveryOutcome::Acknowledged { .. }
    ));
}

fn tracked(delivery_id: &str, epoch: u64) -> TrackedPurpose {
    TrackedPurpose {
        epoch,
        purpose: PublishPurpose::Delivery(DeliveryId::new(delivery_id).expect("delivery identity")),
    }
}

fn test_session() -> (
    Session<serde_json::Value, ()>,
    FakeTargetHost<serde_json::Value, ()>,
) {
    let bridge_id = BridgeId::new("bridge-a").expect("bridge identity");
    let target_id = TargetInstanceId::new("main").expect("target identity");
    let topics = TopicNamespace::new(Environment::Demo, &bridge_id).expect("topic namespace");
    let target = MqttTarget::new(
        topics.clone(),
        MqttSettings {
            endpoint: BrokerEndpoint {
                host: "127.0.0.1".to_owned(),
                port: 9,
                tls: false,
            },
            credentials_file: None,
            target_instance_id: target_id,
            configuration_revision: 1,
            client_id: "uob-test-client".to_owned(),
            command_principal: uob_contracts::PrincipalId::new("mqtt-target:main")
                .expect("principal"),
            home_assistant_discovery: false,
            profile: crate::configuration::MqttProfile::Standard,
        },
        MqttRuntimeOptions::default(),
    );
    let built = FakeTargetHost::<serde_json::Value, ()>::build(
        HostCapacities {
            deliveries: 1,
            commands: 1,
            reports: 4,
            diagnostics: 4,
        },
        Arc::new(UnsupportedQueryPort),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 4,
            maximum_in_flight_commands: 1,
            maximum_command_bytes: 1024,
        },
        UtcTimestamp::new(OffsetDateTime::now_utc() + Duration::from_secs(5)),
    )
    .expect("fake target host");
    let mut session = Session::new(target, built.context, None);
    session.connected = true;
    session.connected_before = true;
    session.epoch = 1;
    (session, built.host)
}
