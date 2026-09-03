#![allow(dead_code)]

use std::{io, net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

pub mod fixtures;
pub mod harness;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub struct TestBroker {
    listener: TcpListener,
    address: SocketAddr,
}

impl TestBroker {
    pub async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test broker");
        let address = listener.local_addr().expect("test broker address");
        Self { listener, address }
    }

    pub fn url(&self) -> String {
        format!("mqtt://{}", self.address)
    }

    pub async fn accept(&self, session_present: bool) -> BrokerConnection {
        let (stream, _) = tokio::time::timeout(IO_TIMEOUT, self.listener.accept())
            .await
            .expect("MQTT client connection timeout")
            .expect("accept MQTT client");
        let mut connection = BrokerConnection {
            stream,
            connect: None,
        };
        let (header, bytes) = connection.next_frame().await.expect("CONNECT frame");
        assert_eq!(header >> 4, 1, "first MQTT packet must be CONNECT");
        let connect = parse_connect(&bytes);
        connection
            .write_frame(&[0x20, 0x02, u8::from(session_present), 0x00])
            .await;
        connection.connect = Some(connect);
        connection
    }

    pub async fn expect_no_connection(&self, duration: Duration) {
        assert!(
            tokio::time::timeout(duration, self.listener.accept())
                .await
                .is_err(),
            "MQTT client unexpectedly reconnected"
        );
    }
}

pub struct BrokerConnection {
    stream: TcpStream,
    connect: Option<ConnectPacket>,
}

impl BrokerConnection {
    pub fn connect(&self) -> &ConnectPacket {
        self.connect.as_ref().expect("parsed CONNECT packet")
    }

    pub async fn next_publish(&mut self) -> Publication {
        loop {
            let (header, bytes) = self.next_frame().await.expect("MQTT packet");
            match header >> 4 {
                3 => return parse_publish(header, &bytes),
                12 => self.write_frame(&[0xD0, 0x00]).await,
                packet_type => panic!("expected PUBLISH, received MQTT type {packet_type}"),
            }
        }
    }

    pub async fn acknowledge(&mut self, publication: &Publication) {
        let packet_id = publication.packet_id.expect("QoS 1 packet identifier");
        let [high, low] = packet_id.to_be_bytes();
        self.write_frame(&[0x40, 0x02, high, low]).await;
    }

    pub async fn acknowledge_and_close(mut self, publication: &Publication) {
        self.acknowledge(publication).await;
        self.stream
            .shutdown()
            .await
            .expect("close broker connection after PUBACK");
    }

    pub async fn expect_disconnect(&mut self) {
        let (header, bytes) = self.next_frame().await.expect("DISCONNECT frame");
        assert_eq!(header, 0xE0);
        assert!(bytes.is_empty());
    }

    async fn next_frame(&mut self) -> Option<(u8, Vec<u8>)> {
        tokio::time::timeout(IO_TIMEOUT, read_frame(&mut self.stream))
            .await
            .expect("MQTT packet timeout")
            .expect("read MQTT frame")
    }

    async fn write_frame(&mut self, bytes: &[u8]) {
        tokio::time::timeout(IO_TIMEOUT, self.stream.write_all(bytes))
            .await
            .expect("MQTT write timeout")
            .expect("write MQTT frame");
    }
}

#[derive(Debug)]
pub struct ConnectPacket {
    pub protocol_level: u8,
    pub clean_session: bool,
    pub client_id: String,
    pub will: Option<Publication>,
}

#[derive(Clone, Debug)]
pub struct Publication {
    pub topic: String,
    pub payload: Vec<u8>,
    pub packet_id: Option<u16>,
    pub qos: u8,
    pub retain: bool,
    pub duplicate: bool,
}

async fn read_frame(stream: &mut TcpStream) -> io::Result<Option<(u8, Vec<u8>)>> {
    let header = match stream.read_u8().await {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut remaining = 0_usize;
    let mut multiplier = 1_usize;
    for _ in 0..4 {
        let byte = stream.read_u8().await?;
        remaining = remaining
            .checked_add(usize::from(byte & 0x7f) * multiplier)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "remaining length"))?;
        if byte & 0x80 == 0 {
            let mut body = vec![0; remaining];
            stream.read_exact(&mut body).await?;
            return Ok(Some((header, body)));
        }
        multiplier = multiplier
            .checked_mul(128)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "remaining length"))?;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "overlong remaining length",
    ))
}

fn parse_connect(bytes: &[u8]) -> ConnectPacket {
    let mut cursor = 0;
    assert_eq!(mqtt_bytes(bytes, &mut cursor), b"MQTT");
    let protocol_level = take_byte(bytes, &mut cursor);
    let flags = take_byte(bytes, &mut cursor);
    cursor += 2;
    let client_id = mqtt_text(bytes, &mut cursor);
    let will = if flags & 0x04 != 0 {
        let topic = mqtt_text(bytes, &mut cursor);
        let payload = mqtt_bytes(bytes, &mut cursor).to_vec();
        Some(Publication {
            topic,
            payload,
            packet_id: None,
            qos: (flags >> 3) & 0x03,
            retain: flags & 0x20 != 0,
            duplicate: false,
        })
    } else {
        None
    };
    ConnectPacket {
        protocol_level,
        clean_session: flags & 0x02 != 0,
        client_id,
        will,
    }
}

fn parse_publish(header: u8, bytes: &[u8]) -> Publication {
    let mut cursor = 0;
    let topic = mqtt_text(bytes, &mut cursor);
    let qos = (header >> 1) & 0x03;
    let packet_id = (qos > 0).then(|| {
        let id = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]);
        cursor += 2;
        id
    });
    Publication {
        topic,
        payload: bytes[cursor..].to_vec(),
        packet_id,
        qos,
        retain: header & 0x01 != 0,
        duplicate: header & 0x08 != 0,
    }
}

fn mqtt_text(bytes: &[u8], cursor: &mut usize) -> String {
    String::from_utf8(mqtt_bytes(bytes, cursor).to_vec()).expect("MQTT UTF-8 string")
}

fn mqtt_bytes<'a>(bytes: &'a [u8], cursor: &mut usize) -> &'a [u8] {
    let length = usize::from(u16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]));
    *cursor += 2;
    let value = &bytes[*cursor..*cursor + length];
    *cursor += length;
    value
}

fn take_byte(bytes: &[u8], cursor: &mut usize) -> u8 {
    let value = bytes[*cursor];
    *cursor += 1;
    value
}
