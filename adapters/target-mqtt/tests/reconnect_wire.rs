mod support;

use std::time::Duration;

use serde_json::Value;
use uob_application::{DeliveryOutcome, TargetDiagnostic, TargetHealthState};
use uob_contracts::Environment;
use uob_mqtt_target_adapter::MqttRuntimeOptions;
use uob_target_conformance::{HostCapacities, HostError};

use support::{
    BrokerConnection, Publication, TestBroker,
    fixtures::{event_delivery, snapshot_delivery},
    harness::{RunningTarget, standard_capacities, start_target},
};

#[tokio::test]
async fn reconnect_to_a_new_session_republishes_cached_state_without_duplicate_reports() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(&broker, "bridge-a", fast_runtime(4), standard_capacities());
    let mut first = broker.accept(false).await;
    acknowledge_online(&mut first, "bridge-a").await;

    target
        .host
        .try_deliver(snapshot_delivery("bridge-a", "station-a", "state-1"))
        .expect("state delivery");
    let state = first.next_publish().await;
    assert_publish(&state, "uob/v1/demo/bridge-a/state/station-a", true);
    first.acknowledge(&state).await;
    assert_acknowledged(next_report(&mut target).await.outcome);

    drop(first);
    let mut second = broker.accept(false).await;
    acknowledge_online(&mut second, "bridge-a").await;
    let replay = second.next_publish().await;
    assert_publish(&replay, "uob/v1/demo/bridge-a/state/station-a", true);
    second.acknowledge(&replay).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err(),
        "internal availability/state replay must not create host delivery reports"
    );

    graceful_shutdown(&mut target, &mut second, "bridge-a").await;
}

#[tokio::test]
async fn puback_buffered_immediately_before_eof_is_reported_as_acknowledged() {
    let broker = TestBroker::bind().await;
    let mut target = start_target(&broker, "bridge-a", fast_runtime(4), standard_capacities());
    let mut first = broker.accept(false).await;
    acknowledge_online(&mut first, "bridge-a").await;

    target
        .host
        .try_deliver(event_delivery(
            "bridge-a",
            "station-a",
            "acked-at-eof",
            "event-at-eof",
            Environment::Demo,
        ))
        .expect("event delivery");
    let publication = first.next_publish().await;
    first.acknowledge_and_close(&publication).await;

    let report = next_report(&mut target).await;
    assert_eq!(report.delivery_id.as_str(), "acked-at-eof");
    assert_acknowledged(report.outcome);

    let mut second = broker.accept(false).await;
    acknowledge_online(&mut second, "bridge-a").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.host.next_report())
            .await
            .is_err(),
        "reconnect must not duplicate the broker ACK report"
    );
    graceful_shutdown(&mut target, &mut second, "bridge-a").await;
}

#[tokio::test]
async fn new_session_cancels_a_queued_unwritten_publish_in_packet_id_lockstep() {
    let broker = TestBroker::bind().await;
    let capacities = HostCapacities {
        deliveries: 1,
        commands: 1,
        reports: 4,
        diagnostics: 4,
    };
    let mut target = start_target(&broker, "bridge-a", fast_runtime(1), capacities);
    let mut first = broker.accept(false).await;
    let online = first.next_publish().await;
    assert_publish(&online, "uob/v1/demo/bridge-a/availability", true);
    // The unacknowledged internal publication fills rumqttc's sole in-flight slot. The delivery
    // below is accepted into its bounded request channel but cannot yield Outgoing::Publish.
    target
        .host
        .try_deliver(event_delivery(
            "bridge-a",
            "station-a",
            "queued-before-write",
            "event-before-write",
            Environment::Demo,
        ))
        .expect("queued delivery");
    let probe_id = enqueue_after_first_is_consumed(&target).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first.next_publish())
            .await
            .is_err(),
        "in-flight bound must keep the host delivery unwritten"
    );

    drop(first);
    let mut second = broker.accept(false).await;
    let report = next_report(&mut target).await;
    assert_eq!(report.delivery_id.as_str(), "queued-before-write");
    assert_eq!(
        report.outcome,
        DeliveryOutcome::Uncertain {
            reason: "mqtt.session_lost_before_ack".to_owned(),
        }
    );

    acknowledge_online(&mut second, "bridge-a").await;
    let probe = second.next_publish().await;
    assert_publish(
        &probe,
        &format!("uob/v1/demo/bridge-a/events/station-a/event-probe-{probe_id}"),
        false,
    );
    second.acknowledge(&probe).await;
    assert_acknowledged(next_report(&mut target).await.outcome);
    graceful_shutdown(&mut target, &mut second, "bridge-a").await;
}

#[tokio::test]
async fn unavailable_broker_keeps_ingress_bounded_and_shutdown_responsive() {
    let broker = TestBroker::bind().await;
    let capacities = HostCapacities {
        deliveries: 1,
        commands: 1,
        reports: 1,
        diagnostics: 2,
    };
    let mut target = start_target(&broker, "bridge-a", fast_runtime(1), capacities);

    target
        .host
        .try_deliver(snapshot_delivery("bridge-a", "station-a", "queued-1"))
        .expect("first bounded delivery");
    assert_eq!(
        target
            .host
            .try_deliver(snapshot_delivery("bridge-a", "station-b", "queued-2")),
        Err(HostError::Full)
    );
    tokio::task::yield_now().await;
    let diagnostic = tokio::time::timeout(Duration::from_secs(3), target.host.next_diagnostic())
        .await
        .expect("reconnect diagnostic timeout")
        .expect("diagnostic channel");
    assert!(matches!(
        diagnostic,
        TargetDiagnostic::Health(health) if health.state == TargetHealthState::Reconnecting
    ));

    target.host.request_shutdown();
    let result = tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .expect("bounded shutdown timeout")
        .expect("target task join");
    assert!(result.is_ok(), "target shutdown failed: {result:?}");
}

#[tokio::test]
async fn connected_peer_that_withholds_puback_is_reset_after_bounded_no_progress() {
    let broker = TestBroker::bind().await;
    let capacities = HostCapacities {
        deliveries: 1,
        commands: 1,
        reports: 4,
        diagnostics: 8,
    };
    let runtime = MqttRuntimeOptions {
        no_progress_timeout: Duration::from_millis(300),
        ..fast_runtime(2)
    };
    let mut target = start_target(&broker, "bridge-a", runtime, capacities);
    let mut stalled = broker.accept(false).await;
    let online = stalled.next_publish().await;
    assert_publish(&online, "uob/v1/demo/bridge-a/availability", true);

    target
        .host
        .try_deliver(event_delivery(
            "bridge-a",
            "station-a",
            "slow-peer-1",
            "slow-peer-event-1",
            Environment::Demo,
        ))
        .expect("first slow-peer delivery");
    let probe_id = enqueue_after_first_is_consumed(&target).await;
    assert_eq!(
        target.host.try_deliver(event_delivery(
            "bridge-a",
            "station-a",
            "slow-peer-overflow",
            "slow-peer-overflow",
            Environment::Demo,
        )),
        Err(HostError::Full),
        "host ingress must remain bounded while the broker makes no progress"
    );

    await_health_reason(&mut target, "mqtt.protocol_no_progress").await;
    drop(stalled);
    let mut resumed = broker.accept(true).await;
    acknowledge_online(&mut resumed, "bridge-a").await;

    let first_topic = "uob/v1/demo/bridge-a/events/station-a/slow-peer-event-1";
    let probe_topic = format!("uob/v1/demo/bridge-a/events/station-a/event-probe-{probe_id}");
    let online_topic = "uob/v1/demo/bridge-a/availability";
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..3 {
        let publication = resumed.next_publish().await;
        match publication.topic.as_str() {
            topic if topic == first_topic || topic == probe_topic => {
                assert_publish(&publication, topic, false);
            }
            topic if topic == online_topic => {
                assert_publish(&publication, topic, true);
                assert_eq!(json(&publication)["status"], "online");
            }
            topic => panic!("unexpected resumed-session publication: {topic}"),
        }
        assert!(
            seen.insert(publication.topic.clone()),
            "resumed publication must occur exactly once"
        );
        resumed.acknowledge(&publication).await;
    }
    assert_eq!(
        seen,
        [first_topic.to_owned(), probe_topic, online_topic.to_owned()]
            .into_iter()
            .collect()
    );
    let mut reports = [
        next_report(&mut target).await,
        next_report(&mut target).await,
    ];
    reports.sort_by(|left, right| left.delivery_id.as_str().cmp(right.delivery_id.as_str()));
    assert_eq!(reports[0].delivery_id.as_str(), format!("probe-{probe_id}"));
    assert_eq!(reports[1].delivery_id.as_str(), "slow-peer-1");
    assert!(
        reports
            .into_iter()
            .all(|report| matches!(report.outcome, DeliveryOutcome::Acknowledged { .. }))
    );
    graceful_shutdown(&mut target, &mut resumed, "bridge-a").await;
}

#[tokio::test]
async fn healthy_idle_connection_does_not_arm_the_no_progress_watchdog() {
    let broker = TestBroker::bind().await;
    let runtime = MqttRuntimeOptions {
        no_progress_timeout: Duration::from_millis(100),
        ..fast_runtime(2)
    };
    let mut target = start_target(&broker, "bridge-a", runtime, standard_capacities());
    let mut peer = broker.accept(false).await;
    acknowledge_online(&mut peer, "bridge-a").await;

    target
        .host
        .try_deliver(event_delivery(
            "bridge-a",
            "station-a",
            "idle-barrier",
            "idle-barrier",
            Environment::Demo,
        ))
        .expect("idle barrier delivery");
    let barrier = peer.next_publish().await;
    peer.acknowledge(&barrier).await;
    assert_acknowledged(next_report(&mut target).await.outcome);

    broker
        .expect_no_connection(Duration::from_millis(250))
        .await;

    graceful_shutdown(&mut target, &mut peer, "bridge-a").await;
}

async fn enqueue_after_first_is_consumed(target: &RunningTarget) -> usize {
    for probe in 1..=100 {
        let result = target.host.try_deliver(event_delivery(
            "bridge-a",
            "station-a",
            &format!("probe-{probe}"),
            &format!("event-probe-{probe}"),
            Environment::Demo,
        ));
        match result {
            Ok(()) => return probe,
            Err(HostError::Full) => tokio::task::yield_now().await,
            Err(error) => panic!("target delivery channel failed: {error}"),
        }
    }
    panic!("target never consumed the first queued delivery");
}

fn fast_runtime(maximum_in_flight_deliveries: usize) -> MqttRuntimeOptions {
    MqttRuntimeOptions {
        request_capacity: 4,
        maximum_in_flight_deliveries,
        connection_timeout_seconds: 1,
        reconnect_initial_backoff: Duration::from_millis(10),
        reconnect_maximum_backoff: Duration::from_millis(40),
        ..MqttRuntimeOptions::default()
    }
}

async fn acknowledge_online(peer: &mut BrokerConnection, bridge: &str) {
    let online = peer.next_publish().await;
    assert_publish(&online, &format!("uob/v1/demo/{bridge}/availability"), true);
    assert_eq!(json(&online)["status"], "online");
    peer.acknowledge(&online).await;
}

async fn graceful_shutdown(target: &mut RunningTarget, peer: &mut BrokerConnection, bridge: &str) {
    target.host.request_shutdown();
    let offline = peer.next_publish().await;
    assert_publish(
        &offline,
        &format!("uob/v1/demo/{bridge}/availability"),
        true,
    );
    assert_eq!(json(&offline)["status"], "offline");
    peer.acknowledge(&offline).await;
    peer.expect_disconnect().await;
    let result = tokio::time::timeout(Duration::from_secs(2), &mut target.task)
        .await
        .expect("target shutdown timeout")
        .expect("target task join");
    assert!(result.is_ok(), "target shutdown failed: {result:?}");
}

async fn next_report(target: &mut RunningTarget) -> uob_application::DeliveryReport {
    tokio::time::timeout(Duration::from_secs(2), target.host.next_report())
        .await
        .expect("delivery report timeout")
        .expect("delivery report channel")
}

async fn await_health_reason(target: &mut RunningTarget, reason: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let diagnostic = target
                .host
                .next_diagnostic()
                .await
                .expect("diagnostic channel");
            if matches!(
                diagnostic,
                TargetDiagnostic::Health(health)
                    if health.state == TargetHealthState::Degraded
                        && health.reason.as_deref() == Some(reason)
            ) {
                return;
            }
        }
    })
    .await
    .expect("health diagnostic timeout");
}

fn assert_acknowledged(outcome: DeliveryOutcome) {
    assert!(matches!(
        outcome,
        DeliveryOutcome::Acknowledged { scope, .. } if scope.0 == "mqtt.broker_received"
    ));
}

fn assert_publish(publication: &Publication, topic: &str, retained: bool) {
    assert_eq!(publication.topic, topic);
    assert_eq!(publication.qos, 1);
    assert_eq!(publication.retain, retained);
}

fn json(publication: &Publication) -> Value {
    serde_json::from_slice(&publication.payload).expect("JSON publication")
}
