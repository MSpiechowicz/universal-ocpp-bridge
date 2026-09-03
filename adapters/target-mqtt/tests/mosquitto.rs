mod support;

use std::time::Duration;

use rumqttc::{
    AsyncClient, Broker, Event, EventLoop, Incoming, MqttOptions, Publish, PublishOptions, QoS,
};
use uob_application::DeliveryOutcome;
use uob_contracts::{CommandResult, Environment, EventEnvelope, TraceRecord};
use uob_mqtt_target_adapter::MqttRuntimeOptions;
use url::Url;

use support::{
    fixtures::{TestEvent, event_delivery, result_delivery, snapshot_delivery, trace_delivery},
    harness::{standard_capacities, start_target_url},
};

const MOSQUITTO_URL: &str = "UOB_MQTT_MOSQUITTO_URL";

#[tokio::test]
#[ignore = "requires a real Mosquitto broker named by UOB_MQTT_MOSQUITTO_URL"]
async fn real_mosquitto_preserves_qos1_retained_state_and_exact_namespace() {
    let broker_url = std::env::var(MOSQUITTO_URL).expect("set UOB_MQTT_MOSQUITTO_URL");
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let bridge = format!("mosquitto-{nonce}");
    let base = format!("uob/v1/demo/{bridge}");
    let availability_topic = format!("{base}/availability");
    let state_topic = format!("{base}/state/station-a");
    let wildcard = format!("uob/v1/+/{bridge}/#");

    let mut observer =
        Observer::connect(&broker_url, &format!("observer-a-{nonce}"), &wildcard).await;
    let mut target = start_target_url(
        &broker_url,
        &bridge,
        MqttRuntimeOptions::default(),
        standard_capacities(),
    );
    let online = observer.next_publish().await;
    assert_eq!(topic(&online), availability_topic);
    assert_eq!(online.qos, QoS::AtLeastOnce);
    assert_eq!(json(&online)["status"], "online");

    target
        .host
        .try_deliver(snapshot_delivery(&bridge, "station-a", "state-1"))
        .expect("snapshot delivery");
    let state = observer.next_publish().await;
    assert_eq!(topic(&state), state_topic);
    assert_eq!(state.qos, QoS::AtLeastOnce);
    serde_json::from_slice::<uob_contracts::StationSnapshot>(&state.payload)
        .expect("canonical snapshot schema");
    let report = timeout_report(&mut target).await;
    assert!(matches!(
        report.outcome,
        DeliveryOutcome::Acknowledged { scope, .. } if scope.0 == "mqtt.broker_received"
    ));

    assert_transient_outputs(&mut target, &mut observer, &bridge, &base).await;
    target
        .host
        .try_deliver(event_delivery(
            &bridge,
            "station-a",
            "wrong-environment",
            "production-event",
            Environment::Production,
        ))
        .expect("environment-mismatch delivery");
    assert_eq!(
        timeout_report(&mut target).await.outcome,
        DeliveryOutcome::PermanentFailure {
            reason: "mqtt.canonical_identity_mismatch".to_owned(),
        }
    );
    observer.expect_no_publish(Duration::from_millis(200)).await;

    // A second connection with the trusted target client ID makes real Mosquitto replace the
    // adapter socket and clear the old session. Continued polling must reconnect the adapter and
    // republish online availability plus its bounded retained-state cache.
    force_new_session(&broker_url, &format!("uob-v1-demo-{bridge}-main")).await;
    let forced_offline = observer.next_publish().await;
    assert_eq!(topic(&forced_offline), availability_topic);
    assert_eq!(json(&forced_offline)["status"], "offline");
    let reconnected_online = observer.next_publish().await;
    assert_eq!(topic(&reconnected_online), availability_topic);
    assert_eq!(json(&reconnected_online)["status"], "online");
    let replayed_state = observer.next_publish().await;
    assert_eq!(topic(&replayed_state), state_topic);
    assert_eq!(replayed_state.qos, QoS::AtLeastOnce);

    drop(observer);
    let mut retained = Observer::connect(
        &broker_url,
        &format!("observer-b-{nonce}"),
        &format!("{base}/#"),
    )
    .await;
    let first = retained.next_publish().await;
    let second = retained.next_publish().await;
    let publications = [&first, &second];
    assert!(publications.iter().all(|publication| publication.retain));
    assert!(
        publications
            .iter()
            .any(|publication| topic(publication) == availability_topic)
    );
    assert!(
        publications
            .iter()
            .any(|publication| topic(publication) == state_topic)
    );
    retained.expect_no_publish(Duration::from_millis(200)).await;

    target.host.request_shutdown();
    let offline = retained.next_publish().await;
    assert_eq!(topic(&offline), availability_topic);
    assert_eq!(json(&offline)["status"], "offline");
    let result = tokio::time::timeout(Duration::from_secs(5), &mut target.task)
        .await
        .expect("target shutdown timeout")
        .expect("target task join");
    assert!(result.is_ok(), "target shutdown failed: {result:?}");

    retained
        .clear_retained(&[availability_topic, state_topic])
        .await;
}

struct Observer {
    client: AsyncClient,
    eventloop: EventLoop,
}

impl Observer {
    async fn connect(url: &str, client_id: &str, topic: &str) -> Self {
        let options = observer_options(url, client_id);
        let (client, eventloop) = AsyncClient::builder(options).capacity(8).build();
        client
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .expect("queue observer subscription");
        let mut observer = Self { client, eventloop };
        observer.wait_for_subscription().await;
        observer
    }

    async fn wait_for_subscription(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    self.eventloop.poll().await.expect("observer MQTT event"),
                    Event::Incoming(Incoming::SubAck(_))
                ) {
                    return;
                }
            }
        })
        .await
        .expect("observer subscription timeout");
    }

    async fn next_publish(&mut self) -> Publish {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Event::Incoming(Incoming::Publish(publication)) =
                    self.eventloop.poll().await.expect("observer MQTT event")
                {
                    return publication;
                }
            }
        })
        .await
        .expect("observer publication timeout")
    }

    async fn expect_no_publish(&mut self, duration: Duration) {
        assert!(
            tokio::time::timeout(duration, async {
                loop {
                    if matches!(
                        self.eventloop.poll().await.expect("observer MQTT event"),
                        Event::Incoming(Incoming::Publish(_))
                    ) {
                        return;
                    }
                }
            })
            .await
            .is_err(),
            "observer received an unexpected MQTT publication"
        );
    }

    async fn clear_retained(&mut self, topics: &[String]) {
        for topic in topics {
            self.client
                .publish(
                    topic.as_str(),
                    Vec::new(),
                    PublishOptions::at_least_once().retained(),
                )
                .await
                .expect("queue retained cleanup");
        }
        let mut acknowledgements = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            while acknowledgements < topics.len() {
                if matches!(
                    self.eventloop.poll().await.expect("cleanup MQTT event"),
                    Event::Incoming(Incoming::PubAck(_))
                ) {
                    acknowledgements += 1;
                }
            }
        })
        .await
        .expect("retained cleanup timeout");
    }
}

async fn force_new_session(url: &str, target_client_id: &str) {
    let options = observer_options(url, target_client_id);
    let (_client, mut eventloop) = AsyncClient::builder(options).capacity(1).build();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                eventloop.poll().await.expect("takeover MQTT event"),
                Event::Incoming(Incoming::ConnAck(_))
            ) {
                return;
            }
        }
    })
    .await
    .expect("target client-ID takeover timeout");
}

fn observer_options(url: &str, client_id: &str) -> MqttOptions {
    let parsed = Url::parse(url).expect("Mosquitto URL");
    assert_eq!(
        parsed.scheme(),
        "mqtt",
        "test broker must be isolated plaintext"
    );
    let host = parsed.host_str().expect("Mosquitto host");
    let port = parsed.port().unwrap_or(1883);
    let mut options = MqttOptions::new(client_id, Broker::tcp(host, port));
    options.set_clean_session(true).set_keep_alive(5);
    options
}

async fn timeout_report(
    target: &mut support::harness::RunningTarget,
) -> uob_application::DeliveryReport {
    tokio::time::timeout(Duration::from_secs(5), target.host.next_report())
        .await
        .expect("delivery report timeout")
        .expect("delivery report channel")
}

async fn assert_transient_outputs(
    target: &mut support::harness::RunningTarget,
    observer: &mut Observer,
    bridge: &str,
    base: &str,
) {
    let deliveries = [
        event_delivery(
            bridge,
            "station-a",
            "event-delivery",
            "event-a",
            Environment::Demo,
        ),
        result_delivery(bridge, "station-a", "result-delivery", "request-a"),
        trace_delivery(bridge, "station-a", "trace-delivery", "trace-a"),
    ];
    let topics = [
        format!("{base}/events/station-a/event-a"),
        format!("{base}/results/station-a/request-a"),
        format!("{base}/traces/station-a/trace-a"),
    ];
    for (index, (delivery, expected_topic)) in deliveries.into_iter().zip(topics).enumerate() {
        target
            .host
            .try_deliver(delivery)
            .expect("transient canonical delivery");
        let publication = observer.next_publish().await;
        assert_eq!(topic(&publication), expected_topic);
        assert_eq!(publication.qos, QoS::AtLeastOnce);
        assert!(!publication.retain, "transient output must not be retained");
        match index {
            0 => {
                serde_json::from_slice::<EventEnvelope<TestEvent>>(&publication.payload)
                    .expect("canonical event schema");
            }
            1 => {
                serde_json::from_slice::<CommandResult>(&publication.payload)
                    .expect("canonical result schema");
            }
            2 => {
                serde_json::from_slice::<TraceRecord>(&publication.payload)
                    .expect("canonical trace schema");
            }
            _ => unreachable!(),
        }
        assert_eq!(json(&publication)["schema_version"]["major"], 1);
        assert!(matches!(
            timeout_report(target).await.outcome,
            DeliveryOutcome::Acknowledged { scope, .. }
                if scope.0 == "mqtt.broker_received"
        ));
    }
}

fn json(publication: &Publish) -> serde_json::Value {
    serde_json::from_slice(&publication.payload).expect("JSON publication")
}

fn topic(publication: &Publish) -> &str {
    std::str::from_utf8(&publication.topic).expect("UTF-8 MQTT topic")
}
