use super::{BASE, Host};
use crate::probe;
use rumqttc::{
    AsyncClient, Broker, Event, EventLoop, Incoming, MqttOptions, PublishOptions, QoS, Transport,
};
use std::{path::Path, time::Duration};
use uob_application::OperationalStore;
use uob_contracts::{BridgeId, RequestId, ResourceRef, StationId};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} missing"))
}
fn password(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

pub async fn reject_wrong_tls_and_credentials(demo: &probe::Demo) {
    let url = env("UOB_MQTT_BROKER_URL");
    let ca = env("UOB_MQTT_CA_FILE");
    let wrong_ca = Path::new(&ca).with_file_name("server.crt");
    let user = env("UOB_MQTT_CLIENT_USER");
    let operator_password = env("UOB_MQTT_CLIENT_PASSWORD_FILE");
    let reader_password = env("UOB_MQTT_READER_PASSWORD_FILE");
    let wrong_certificate = probe::run(
        &url,
        &wrong_ca,
        &user,
        Path::new(&operator_password),
        demo,
        false,
        false,
    )
    .await;
    assert!(
        wrong_certificate.as_ref().is_err_and(|error| {
            let reason = error.to_string();
            reason.contains("connection failed") || reason.contains("connection refused")
        }),
        "wrong CA was not rejected during MQTT/TLS connection"
    );
    let wrong_credential = probe::run(
        &url,
        Path::new(&ca),
        &user,
        Path::new(&reader_password),
        demo,
        false,
        false,
    )
    .await;
    assert!(
        wrong_credential.as_ref().is_err_and(|error| {
            let reason = error.to_string();
            reason.contains("connection failed") || reason.contains("connection refused")
        }),
        "bad credential was not rejected during MQTT connection"
    );
}

pub async fn reject_reader_command(host: &Host, _demo: &probe::Demo) {
    let url = url::Url::parse(&env("UOB_MQTT_BROKER_URL")).unwrap();
    let ca = std::fs::read(env("UOB_MQTT_CA_FILE")).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let (reader, mut reader_loop) = AsyncClient::builder(options(
        "reader",
        "UOB_MQTT_READER_USER",
        "UOB_MQTT_READER_PASSWORD_FILE",
        &url,
        &ca,
    ))
    .capacity(8)
    .build();
    connected(&mut reader_loop, deadline).await;

    subscribe_reader(&reader, &mut reader_loop, deadline).await;

    let station = format!("absent-{}", uuid::Uuid::new_v4());
    let resource = ResourceRef {
        bridge_id: BridgeId::new("site-01").unwrap(),
        station_id: StationId::new(station.clone()).unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let reader_id = format!("reader-denied-{}", uuid::Uuid::new_v4());
    let marker_id = format!("operator-marker-{}", uuid::Uuid::new_v4());
    let reader_result = format!("{BASE}/results/{station}/{reader_id}");
    let marker_result = format!("{BASE}/results/{station}/{marker_id}");
    let expired_at = (time::OffsetDateTime::now_utc() - time::Duration::minutes(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let valid_until = (time::OffsetDateTime::now_utc() + time::Duration::minutes(5))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let command = |request_id: &str, expires_at: &str| {
        serde_json::json!({
            "schema_version":{"major":1,"revision":0},
            "request_id":request_id,"correlation_id":request_id,
            "resource":resource,
            "operation":{"kind":"start","parameters":{"authorization_reference":null}},
            "expires_at":expires_at,
        })
    };
    // Neither request targets the simulator: a broken ACL cannot start charging or
    // change its expected command sequence. The operator marker is also expired.
    reader
        .publish(
            format!("{BASE}/commands/{station}/{reader_id}"),
            serde_json::to_vec(&command(&reader_id, &valid_until)).unwrap(),
            PublishOptions::new(QoS::AtLeastOnce),
        )
        .await
        .unwrap();
    reader_puback(&mut reader_loop, &reader_result, &marker_result).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let (operator, mut operator_loop) = AsyncClient::builder(options(
        "operator",
        "UOB_MQTT_CLIENT_USER",
        "UOB_MQTT_CLIENT_PASSWORD_FILE",
        &url,
        &ca,
    ))
    .capacity(8)
    .build();
    connected(&mut operator_loop, deadline).await;
    operator
        .publish(
            format!("{BASE}/commands/{station}/{marker_id}"),
            serde_json::to_vec(&command(&marker_id, &expired_at)).unwrap(),
            PublishOptions::new(QoS::AtLeastOnce),
        )
        .await
        .unwrap();
    loop {
        if let Event::Incoming(Incoming::PubAck(_)) =
            tokio::time::timeout_at(deadline, operator_loop.poll())
                .await
                .unwrap()
                .unwrap()
        {
            break;
        }
    }

    observe_acl(&mut reader_loop, deadline, &reader_result, &marker_result).await;
    assert!(
        host.store
            .command_by_request_id(RequestId::new(reader_id).unwrap())
            .await
            .unwrap()
            .is_none(),
        "reader's forbidden command reached durable coordinator"
    );
}

async fn subscribe_reader(
    reader: &AsyncClient,
    loopback: &mut EventLoop,
    deadline: tokio::time::Instant,
) {
    // Mosquitto may grant this MQTT 3.1.1 SUBSCRIBE while filtering each PUBLISH.
    reader
        .subscribe(format!("{BASE}/commands/+/+"), QoS::AtLeastOnce)
        .await
        .unwrap();
    subscribed(loopback, deadline, false).await;
    reader
        .subscribe(format!("{BASE}/results/+/+"), QoS::AtLeastOnce)
        .await
        .unwrap();
    subscribed(loopback, deadline, true).await;
}

async fn reader_puback(loopback: &mut EventLoop, reader_result: &str, marker_result: &str) {
    let mut sent = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        match tokio::time::timeout_at(deadline, loopback.poll()).await {
            Ok(Ok(Event::Outgoing(rumqttc::Outgoing::Publish(_)))) => sent = true,
            Ok(Ok(Event::Incoming(Incoming::PubAck(_)))) => break,
            Ok(Ok(event)) => {
                observe_reader(event, reader_result, marker_result);
            }
            Ok(Err(error)) => panic!("reader disconnected during forbidden publication: {error}"),
            Err(error) => {
                panic!("reader's forbidden publication was not acknowledged by broker: {error}")
            }
        }
    }
    assert!(sent, "reader's command was not transmitted to broker");
}

async fn observe_acl(
    loopback: &mut EventLoop,
    deadline: tokio::time::Instant,
    reader_result: &str,
    marker_result: &str,
) {
    // The operator marker's result proves the bridge processed an allowed command
    // and this same reader received authorized output, not merely an idle socket.
    loop {
        let event = tokio::time::timeout_at(deadline, loopback.poll())
            .await
            .unwrap()
            .unwrap();
        if observe_reader(event, reader_result, marker_result) {
            break;
        }
    }
    let quiet_until = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        match tokio::time::timeout_at(quiet_until, loopback.poll()).await {
            Ok(Ok(event)) => {
                observe_reader(event, reader_result, marker_result);
            }
            Ok(Err(error)) => panic!("reader disconnected during ACL observation: {error}"),
            Err(_) => break,
        }
    }
}

fn options(role: &str, user: &str, password_file: &str, url: &url::Url, ca: &[u8]) -> MqttOptions {
    let mut options = MqttOptions::new(
        format!("issue88-{role}-{}", uuid::Uuid::new_v4()),
        Broker::tcp(url.host_str().unwrap(), url.port().unwrap()),
    );
    options.set_transport(Transport::tls(ca.to_vec(), None, None));
    options.set_credentials(env(user), password(Path::new(&env(password_file))));
    options.set_keep_alive(5);
    options
}

async fn connected(loopback: &mut EventLoop, deadline: tokio::time::Instant) {
    loop {
        if let Event::Incoming(Incoming::ConnAck(ack)) =
            tokio::time::timeout_at(deadline, loopback.poll())
                .await
                .unwrap()
                .unwrap()
        {
            assert_eq!(ack.code, rumqttc::ConnectReturnCode::Success);
            break;
        }
    }
}

async fn subscribed(loopback: &mut EventLoop, deadline: tokio::time::Instant, required: bool) {
    loop {
        if let Event::Incoming(Incoming::SubAck(ack)) =
            tokio::time::timeout_at(deadline, loopback.poll())
                .await
                .unwrap()
                .unwrap()
        {
            if required {
                assert!(
                    ack.return_codes.iter().all(|reason| !matches!(
                        reason,
                        rumqttc::mqttbytes::v4::SubscribeReasonCode::Failure
                    )),
                    "reader cannot observe authorized results"
                );
            }
            break;
        }
    }
}

fn observe_reader(event: Event, reader_result: &str, marker_result: &str) -> bool {
    if let Event::Incoming(Incoming::Publish(publication)) = event {
        assert!(
            !publication
                .topic
                .strip_prefix(BASE.as_bytes())
                .is_some_and(|suffix| suffix.starts_with(b"/commands/")),
            "reader received a command despite read-only ACL"
        );
        assert_ne!(
            publication.topic.as_ref(),
            reader_result.as_bytes(),
            "bridge received reader's forbidden command"
        );
        return publication.topic.as_ref() == marker_result.as_bytes();
    }
    false
}
