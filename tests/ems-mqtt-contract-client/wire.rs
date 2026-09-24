use rumqttc::{
    AsyncClient, Broker, Event, EventLoop, Incoming, MqttOptions, Publish, PublishOptions, QoS,
    Transport,
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    time::Duration,
};
use tokio::time::{Instant, timeout_at};

use super::{Error, Result};

const MAX_PAYLOAD: usize = 256 * 1024;
const MAX_INBOX: usize = 512;
const WAIT: Duration = Duration::from_secs(12);
const RECONNECT_WAIT: Duration = Duration::from_secs(60);
const SUBSCRIPTIONS: [&str; 6] = [
    "availability",
    "state/+",
    "points/+/+",
    "values/+/+",
    "events/+/+",
    "results/+/+",
];

enum Signal {
    Publish(u16),
    PubAck(u16),
    Other,
}

pub struct Peer {
    client: AsyncClient,
    eventloop: EventLoop,
    inbox: BTreeMap<String, VecDeque<Publish>>,
    base: String,
    ever_connected: bool,
    pub reconnects: usize,
    broker_host: String,
    broker_port: u16,
    ca: Vec<u8>,
    username: String,
    password: String,
    pub connected: bool,
    pub subscriptions: usize,
}

impl Peer {
    pub async fn connect(
        broker_url: &str,
        ca_file: &Path,
        username: &str,
        password_file: &Path,
        base: &str,
        allow_remote_exercise: bool,
        exercise: bool,
    ) -> Result<Self> {
        let url = url::Url::parse(broker_url).map_err(|_| Error::new("invalid broker URL"))?;
        if url.scheme() != "mqtts"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(Error::new("broker must be a credential-free mqtts URL"));
        }
        let host = url.host_str().ok_or(Error::new("broker host missing"))?;
        if exercise
            && !allow_remote_exercise
            && !matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        {
            return Err(Error::new(
                "active exercise requires loopback or --allow-remote-exercise",
            ));
        }
        let ca = read_bounded(ca_file, 1024 * 1024)?;
        let password = String::from_utf8(read_bounded(password_file, 64 * 1024)?)
            .map_err(|_| Error::new("invalid password file"))?;
        let password = password.trim_end_matches(['\r', '\n']).to_owned();
        if username.is_empty() || password.is_empty() {
            return Err(Error::new("MQTT credentials missing"));
        }
        let nonce = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        let mut options = MqttOptions::new(
            format!("ems-contract-{}-{nonce}", std::process::id()),
            Broker::tcp(host, url.port().unwrap_or(8883)),
        );
        options.set_transport(Transport::tls(ca.clone(), None, None));
        options.set_credentials(username.to_owned(), password.clone());
        options.set_keep_alive(10);
        options.set_max_packet_size(MAX_PAYLOAD + 65535 + 9, MAX_PAYLOAD + 65535 + 9);
        let (client, eventloop) = AsyncClient::builder(options).capacity(16).build();
        let mut peer = Self {
            client,
            eventloop,
            inbox: BTreeMap::new(),
            connected: false,
            subscriptions: 0,
            base: base.to_owned(),
            reconnects: 0,
            ever_connected: false,
            broker_host: host.to_owned(),
            broker_port: url.port().unwrap_or(8883),
            ca,
            username: username.to_owned(),
            password,
        };
        peer.subscribe_all().await?;
        let deadline = Instant::now() + WAIT;
        while !peer.connected || peer.subscriptions != SUBSCRIPTIONS.len() {
            peer.poll(deadline).await?;
        }
        Ok(peer)
    }

    async fn subscribe_all(&self) -> Result<()> {
        for topic in SUBSCRIPTIONS {
            self.client
                .subscribe(format!("{}/{topic}", self.base), QoS::AtLeastOnce)
                .await
                .map_err(|_| Error::new("subscription queue failed"))?;
        }
        Ok(())
    }

    async fn next_event(&mut self, deadline: Instant) -> Result<Event> {
        loop {
            match timeout_at(deadline, self.eventloop.poll()).await {
                Ok(Ok(event)) => return Ok(event),
                Ok(Err(error)) if self.ever_connected => {
                    self.connected = false;
                    if Instant::now() >= deadline {
                        return Err(Error::owned(format!(
                            "MQTT reconnect deadline exceeded: {error}"
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok(Err(error)) => {
                    return Err(Error::owned(format!("MQTT connection failed: {error}")));
                }
                Err(_) => return Err(Error::new("MQTT connection/reconnect deadline exceeded")),
            }
        }
    }

    async fn poll(&mut self, deadline: Instant) -> Result<Signal> {
        match self.next_event(deadline).await? {
            Event::Incoming(Incoming::ConnAck(ack)) => {
                if ack.code != rumqttc::ConnectReturnCode::Success {
                    return Err(Error::owned(format!(
                        "MQTT connection refused: {:?}",
                        ack.code
                    )));
                }
                if self.ever_connected {
                    self.reconnects += 1;
                    if !ack.session_present {
                        self.subscriptions = 0;
                        self.inbox.clear();
                        self.subscribe_all().await?;
                    }
                }
                self.ever_connected = true;
                self.connected = true;
            }
            Event::Incoming(Incoming::SubAck(ack)) => {
                if ack.return_codes.iter().any(|code| {
                    matches!(code, rumqttc::mqttbytes::v4::SubscribeReasonCode::Failure)
                }) {
                    return Err(Error::new("MQTT subscription rejected by broker"));
                }
                self.subscriptions += 1;
            }
            Event::Incoming(Incoming::Publish(message)) => {
                if message.qos != QoS::AtLeastOnce || message.payload.len() > MAX_PAYLOAD {
                    return Err(Error::new("unexpected MQTT QoS or oversized payload"));
                }
                if self.inbox.values().map(VecDeque::len).sum::<usize>() >= MAX_INBOX {
                    return Err(Error::new("MQTT inbox bound exceeded"));
                }
                let topic = std::str::from_utf8(&message.topic)
                    .map_err(|_| Error::new("invalid MQTT topic"))?
                    .to_owned();
                self.inbox.entry(topic).or_default().push_back(message);
            }
            Event::Outgoing(rumqttc::Outgoing::Publish(pkid)) => return Ok(Signal::Publish(pkid)),
            Event::Incoming(Incoming::PubAck(ack)) => return Ok(Signal::PubAck(ack.pkid)),
            _ => {}
        }
        Ok(Signal::Other)
    }

    pub async fn wait_for(
        &mut self,
        topic: &str,
        predicate: impl Fn(&Publish) -> bool,
    ) -> Result<Publish> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(messages) = self.inbox.get_mut(topic)
                && let Some(index) = messages.iter().position(&predicate)
            {
                return messages
                    .remove(index)
                    .ok_or(Error::new("publication missing"));
            }
            self.poll(deadline).await?;
        }
    }

    pub async fn wait_prefix(
        &mut self,
        prefix: &str,
        predicate: impl Fn(&Publish) -> bool,
    ) -> Result<Publish> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some((topic, index)) = self.inbox.iter().find_map(|(topic, messages)| {
                topic
                    .starts_with(prefix)
                    .then(|| {
                        messages
                            .iter()
                            .position(&predicate)
                            .map(|index| (topic.clone(), index))
                    })
                    .flatten()
            }) {
                return self
                    .inbox
                    .get_mut(&topic)
                    .and_then(|messages| messages.remove(index))
                    .ok_or(Error::new("event missing"));
            }
            self.poll(deadline).await?;
        }
    }

    /// A broker outage must produce a new CONNACK and acknowledged subscriptions. Merely
    /// receiving a cached publication is not evidence of a consumer reconnection.
    pub async fn wait_reconnected(&mut self) -> Result<()> {
        let previous = self.reconnects;
        let deadline = Instant::now() + RECONNECT_WAIT;
        loop {
            self.poll(deadline).await?;
            if self.connected
                && self.reconnects > previous
                && self.subscriptions == SUBSCRIPTIONS.len()
            {
                return Ok(());
            }
        }
    }
    pub async fn force_target_reconnect(&self, base: &str) -> Result<()> {
        let bridge = base
            .strip_prefix("uob/v1/demo/")
            .ok_or(Error::new("unexpected MQTT namespace"))?;
        let mut options = MqttOptions::new(
            format!("uob-v1-demo-{bridge}-main"),
            Broker::tcp(&self.broker_host, self.broker_port),
        );
        options.set_transport(Transport::tls(self.ca.clone(), None, None));
        options.set_credentials(self.username.clone(), self.password.clone());
        options.set_keep_alive(10);
        let (_temporary_client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
        let deadline = Instant::now() + WAIT;
        loop {
            let event = timeout_at(deadline, eventloop.poll())
                .await
                .map_err(|_| Error::new("MQTT target reconnect deadline exceeded"))?
                .map_err(|error| Error::owned(format!("MQTT takeover failed: {error}")))?;
            if let Event::Incoming(Incoming::ConnAck(ack)) = event {
                if ack.code != rumqttc::ConnectReturnCode::Success {
                    return Err(Error::new("MQTT target takeover refused"));
                }
                return Ok(());
            }
        }
    }

    pub async fn publish(&mut self, topic: String, payload: &[u8], retain: bool) -> Result<()> {
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::new("outgoing command exceeds bound"));
        }
        self.client
            .publish(
                topic,
                payload.to_vec(),
                PublishOptions::new(QoS::AtLeastOnce).retain(retain),
            )
            .await
            .map_err(|_| Error::new("MQTT publish queue failed"))?;
        // A queued publish is not a broker acknowledgement. Wait for the matching PUBACK.
        let deadline = Instant::now() + WAIT;
        let mut packet_id = None;
        loop {
            match self.poll(deadline).await? {
                Signal::Publish(pkid) => packet_id = Some(pkid),
                Signal::PubAck(pkid) if Some(pkid) == packet_id => return Ok(()),
                _ => {}
            }
        }
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let metadata = std::fs::metadata(path).map_err(|_| Error::new("credential file unreadable"))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(Error::new("credential file invalid or exceeds bound"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| Error::new("credential file unreadable"))?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("credential file unreadable"))?;
    if bytes.len() as u64 > limit {
        return Err(Error::new("credential file exceeds bound"));
    }
    Ok(bytes)
}
