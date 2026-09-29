use rumqttc::{
    AsyncClient, Broker, Event, EventLoop, Incoming, MqttOptions, Publish, PublishOptions, QoS,
    Transport,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write,
};

use crate::{
    Error, Result,
    config::{Args, Station},
    evidence,
};

const MAX_PAYLOAD: usize = 256 * 1024;
const MAX_QUEUED: usize = 128;

struct Peer {
    client: AsyncClient,
    eventloop: EventLoop,
    inbox: BTreeMap<String, VecDeque<Publish>>,
}

fn segment(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(&mut encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn document(message: &Publish) -> Result<Value> {
    serde_json::from_slice(&message.payload).map_err(|_| Error("invalid MQTT JSON"))
}

impl Peer {
    async fn connect(args: &Args, base: &str) -> Result<Self> {
        let url = url::Url::parse(args.broker_url.as_deref().ok_or(Error("broker missing"))?)
            .map_err(|_| Error("invalid broker endpoint"))?;
        let host = url.host_str().ok_or(Error("broker host missing"))?;
        let port = url
            .port()
            .unwrap_or(if url.scheme() == "mqtts" { 8883 } else { 1883 });
        let mut options = MqttOptions::new(
            format!("compose-{}", uuid::Uuid::new_v4()),
            Broker::tcp(host, port),
        );
        if let Some(ca) = &args.ca {
            options.set_transport(Transport::tls(ca.clone(), None, None));
        }
        options.set_credentials(
            args.mqtt_username
                .clone()
                .ok_or(Error("username missing"))?,
            args.mqtt_password
                .clone()
                .ok_or(Error("password missing"))?,
        );
        options.set_keep_alive(10);
        options.set_max_packet_size(MAX_PAYLOAD + 65535 + 9, MAX_PAYLOAD + 65535 + 9);
        let (client, eventloop) = AsyncClient::builder(options).capacity(16).build();
        let mut peer = Self {
            client,
            eventloop,
            inbox: BTreeMap::new(),
        };
        let subscriptions = [
            format!("{base}/availability"),
            format!("{base}/state/+"),
            format!("{base}/results/+/+"),
            format!("{base}/events/+/+"),
        ];
        for topic in &subscriptions {
            peer.client
                .subscribe(topic, QoS::AtLeastOnce)
                .await
                .map_err(|_| Error("MQTT subscription queue failed"))?;
        }
        let mut acknowledged = 0;
        let mut connected = false;
        while !connected || acknowledged < subscriptions.len() {
            match peer.poll().await? {
                Some(Event::Incoming(Incoming::ConnAck(ack))) => {
                    if ack.code != rumqttc::ConnectReturnCode::Success {
                        return Err(Error("MQTT broker refused connection"));
                    }
                    connected = true;
                }
                Some(Event::Incoming(Incoming::SubAck(ack))) => {
                    if ack.return_codes.iter().any(|code| {
                        matches!(code, rumqttc::mqttbytes::v4::SubscribeReasonCode::Failure)
                    }) {
                        return Err(Error("MQTT subscription refused"));
                    }
                    acknowledged += 1;
                }
                _ => {}
            }
        }
        Ok(peer)
    }

    async fn poll(&mut self) -> Result<Option<Event>> {
        let event = self
            .eventloop
            .poll()
            .await
            .map_err(|_| Error("MQTT connection failed"))?;
        if let Event::Incoming(Incoming::Publish(publication)) = &event {
            if publication.qos != QoS::AtLeastOnce || publication.payload.len() > MAX_PAYLOAD {
                return Err(Error("MQTT publication violates bounds"));
            }
            if self.inbox.values().map(VecDeque::len).sum::<usize>() >= MAX_QUEUED {
                return Err(Error("MQTT evidence queue exceeded bound"));
            }
            let topic =
                std::str::from_utf8(&publication.topic).map_err(|_| Error("invalid MQTT topic"))?;
            self.inbox
                .entry(topic.to_owned())
                .or_default()
                .push_back(publication.clone());
        }
        Ok(Some(event))
    }

    async fn receive(
        &mut self,
        topic: &str,
        predicate: impl Fn(&Publish) -> bool,
    ) -> Result<Publish> {
        loop {
            if let Some(queue) = self.inbox.get_mut(topic)
                && let Some(index) = queue.iter().position(&predicate)
            {
                return queue.remove(index).ok_or(Error("missing MQTT publication"));
            }
            self.poll().await?;
        }
    }

    async fn receive_prefix(
        &mut self,
        prefix: &str,
        predicate: impl Fn(&Publish) -> bool,
    ) -> Result<Publish> {
        loop {
            let matching = self.inbox.iter().find_map(|(topic, queue)| {
                if !topic.starts_with(prefix) {
                    return None;
                }
                queue
                    .iter()
                    .position(&predicate)
                    .map(|index| (topic.clone(), index))
            });
            if let Some((topic, index)) = matching {
                return self
                    .inbox
                    .get_mut(&topic)
                    .and_then(|queue| queue.remove(index))
                    .ok_or(Error("missing MQTT event"));
            }
            self.poll().await?;
        }
    }

    async fn publish(&mut self, topic: String, payload: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(payload).map_err(|_| Error("invalid command JSON"))?;
        if bytes.len() > MAX_PAYLOAD {
            return Err(Error("command exceeds MQTT bound"));
        }
        self.client
            .publish(topic, bytes, PublishOptions::new(QoS::AtLeastOnce))
            .await
            .map_err(|_| Error("MQTT command queue failed"))?;
        let mut published = None;
        loop {
            match self.poll().await? {
                Some(Event::Outgoing(rumqttc::Outgoing::Publish(pkid))) => published = Some(pkid),
                Some(Event::Incoming(Incoming::PubAck(ack))) if published == Some(ack.pkid) => {
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}

pub async fn run(args: &Args) -> Result<()> {
    let base = format!(
        "uob/v1/{}/{}",
        args.config.environment,
        segment(&args.config.bridge_id)
    );
    let mut peer = Peer::connect(args, &base).await?;
    peer.receive(&format!("{base}/availability"), |publication| {
        document(publication).is_ok_and(|doc| {
            doc["status"] == "online"
                && doc["bridge_id"] == args.config.bridge_id
                && doc["target_instance_id"] == args.config.target_instance_id
        })
    })
    .await?;

    for station in &args.config.station {
        let station_topic = format!("{base}/state/{}", segment(&station.id));
        let initial = peer
            .receive(&station_topic, |publication| {
                document(publication)
                    .is_ok_and(|state| evidence::resource(&state, &args.config, station).is_ok())
            })
            .await?;
        let state = document(&initial)?;
        let resource = evidence::resource(&state, &args.config, station)?;
        let old_ids = evidence::baseline(&state)?;
        let evidence = StationEvidence {
            args,
            station,
            resource: &resource,
            old_ids: &old_ids,
        };
        let started = command(
            &mut peer,
            &evidence,
            &base,
            &station_topic,
            "start",
            &station.authorization_reference,
            None,
        )
        .await?;
        command(
            &mut peer,
            &evidence,
            &base,
            &station_topic,
            "stop",
            &started,
            Some(&started),
        )
        .await?;
        println!(
            "charging verified: protocol={} start=native+state+event stop=native+state+event",
            station.protocol
        );
    }
    Ok(())
}
struct StationEvidence<'a> {
    args: &'a Args,
    station: &'a Station,
    resource: &'a Value,
    old_ids: &'a [String],
}

async fn command(
    peer: &mut Peer,
    evidence: &StationEvidence<'_>,
    base: &str,
    station_topic: &str,
    kind: &str,
    parameter: &str,
    started: Option<&str>,
) -> Result<String> {
    let StationEvidence {
        args,
        station,
        resource,
        old_ids,
    } = *evidence;
    let request_id = format!("compose-{}-{kind}", uuid::Uuid::new_v4());
    let expires = (time::OffsetDateTime::now_utc() + time::Duration::minutes(5))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| Error("command expiry unavailable"))?;
    let parameters = if kind == "start" {
        json!({"authorization_reference":parameter})
    } else {
        json!({"transaction_id":parameter})
    };
    let command = json!({"schema_version":{"major":1,"revision":0},
        "request_id":request_id,"correlation_id":request_id,"resource":resource,
        "operation":{"kind":kind,"parameters":parameters},"expires_at":expires});
    let encoded_station = segment(&station.id);
    let result_topic = format!("{base}/results/{encoded_station}/{}", segment(&request_id));
    peer.publish(
        format!("{base}/commands/{encoded_station}/{}", segment(&request_id)),
        &command,
    )
    .await?;
    let result = peer.receive(&result_topic, |_| true).await?;
    if result.retain {
        return Err(Error("native command result must not be retained"));
    }
    if !evidence::accepted(
        &document(&result)?,
        &args.config,
        station,
        resource,
        &request_id,
        Some(&request_id),
    )? {
        return Err(Error("native protocol did not accept command"));
    }
    let updated = peer
        .receive(station_topic, |message| {
            document(message).is_ok_and(|state| {
                evidence::effect(&state, &args.config, station, resource, old_ids, started)
                    .is_ok_and(|found| found.is_some())
            })
        })
        .await?;
    if updated.retain {
        return Err(Error(
            "command effect was only retained state, not live update",
        ));
    }
    let id = evidence::effect(
        &document(&updated)?,
        &args.config,
        station,
        resource,
        old_ids,
        started,
    )?
    .ok_or(Error("native command had no matching transaction effect"))?;
    let event_type = if kind == "start" {
        "transaction.started"
    } else {
        "transaction.ended"
    };
    let prefix = format!("{base}/events/{encoded_station}/");
    let event = peer
        .receive_prefix(&prefix, |message| {
            document(message).is_ok_and(|envelope| {
                envelope["event_type"] == event_type
                    && envelope["payload"]["transaction_id"] == id
                    && envelope["resource"]["bridge_id"] == args.config.bridge_id
                    && envelope["resource"]["station_id"] == station.id
            })
        })
        .await?;
    if event.retain {
        return Err(Error("charging domain event must not be retained"));
    }
    let envelope = document(&event)?;
    let event_id = envelope["event_id"]
        .as_str()
        .ok_or(Error("charging event ID absent"))?;
    if !event.topic.ends_with(segment(event_id).as_bytes()) {
        return Err(Error("charging event topic identity mismatch"));
    }
    Ok(id)
}
