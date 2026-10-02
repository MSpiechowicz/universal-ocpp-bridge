use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;

pub const READ: &str = "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const CONTROL: &str = "uob1.demo.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const PRIVILEGED: &str = "uob1.demo.cccccccccccccccccccccccccccccccc";
pub const SCHEMA: &str = "urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest";
pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
pub type Pending = tokio::task::JoinHandle<reqwest::Response>;

pub struct Fixture {
    pub root: PathBuf,
    management: u16,
    charging: u16,
}

pub struct Process(Child);

impl Process {
    pub async fn expect_configuration_failure(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    assert!(!status.success(), "invalid configuration must fail startup");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("invalid configuration exits before serving");
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Fixture {
    pub fn new() -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let root = std::env::temp_dir().join(format!("uob-composite-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        for (name, value) in [
            ("read", READ),
            ("control", CONTROL),
            ("privileged", PRIVILEGED),
            ("station-a", "station-alpha-secret-12345"),
            ("station-b", "station-bravo-secret-67890"),
        ] {
            let path = root.join(name);
            fs::write(&path, value).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let management = vacant_port();
        let charging = loop {
            let candidate = vacant_port();
            if candidate != management {
                break candidate;
            }
        };
        let config = format!(
            "[bridge]\nid='bridge-1'\nenvironment='demo'\n\
             [management]\nlisten_addr='127.0.0.1:{management}'\n\
             [charging]\nenabled=true\nlisten_addr='127.0.0.1:{charging}'\n\
             state_directory='{}'\nread_grant_file='{}'\ncontrol_grant_file='{}'\nprivileged_grant_file='{}'\n\
             [[charging.stations]]\nid='station-a'\nprotocol='ocpp16j'\nget_composite_schedule=true\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nconnector_id='one'\nnative_connector_id=1\n\
             [[charging.stations.resources]]\nconnector_id='two'\nnative_connector_id=2\n\
             [[charging.stations]]\nid='station-b'\nprotocol='ocpp16j'\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nconnector_id='one'\nnative_connector_id=1\n",
            state.display(),
            root.join("read").display(),
            root.join("control").display(),
            root.join("privileged").display(),
            root.join("station-a").display(),
            root.join("station-b").display(),
        );
        fs::write(root.join("bridge.toml"), config).unwrap();
        Self {
            root,
            management,
            charging,
        }
    }

    pub fn start(&self) -> Process {
        Process(
            Command::new(env!("CARGO_BIN_EXE_uob"))
                .args(["serve", "--config"])
                .arg(self.root.join("bridge.toml"))
                .arg("--no-ui")
                .env_remove("NOTIFY_SOCKET")
                .env_remove("WATCHDOG_USEC")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    pub async fn ready(&self, child: &mut Process) {
        for _ in 0..100 {
            assert!(child.0.try_wait().unwrap().is_none(), "service exited");
            if tokio::net::TcpStream::connect(("127.0.0.1", self.management))
                .await
                .is_ok()
                && tokio::net::TcpStream::connect(("127.0.0.1", self.charging))
                    .await
                    .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("management and charging listeners did not start");
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.management)
    }

    pub async fn station(&self, name: &str) -> Socket {
        self.station_protocol(name, "ocpp1.6").await
    }

    pub async fn station_protocol(&self, name: &str, protocol: &str) -> Socket {
        let token = if name == "station-a" {
            "c3RhdGlvbi1hOnN0YXRpb24tYWxwaGEtc2VjcmV0LTEyMzQ1"
        } else {
            "c3RhdGlvbi1iOnN0YXRpb24tYnJhdm8tc2VjcmV0LTY3ODkw"
        };
        let mut request = format!("ws://127.0.0.1:{}/ocpp/{name}", self.charging)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
        request
            .headers_mut()
            .insert("Authorization", format!("Basic {token}").parse().unwrap());
        connect_async(request).await.unwrap().0
    }

    pub async fn snapshot(&self, client: &reqwest::Client, station: &str) -> Value {
        let response = client
            .get(self.url(&format!("/api/v1/stations/{station}")))
            .bearer_auth(READ)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }

    pub async fn connected(&self, client: &reqwest::Client, station: &str) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let response = client
                    .get(self.url(&format!("/api/v1/stations/{station}")))
                    .bearer_auth(READ)
                    .send()
                    .await
                    .unwrap();
                if response.status() == 200
                    && response.json::<Value>().await.unwrap()["connectivity"]["status"]
                        == "connected"
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("station becomes connected");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn vacant_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(45))
        .build()
        .unwrap()
}

pub async fn receive(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(8), socket.next())
        .await
        .unwrap()
        .expect("connected socket")
        .expect("valid socket frame");
    let Message::Text(text) = message else {
        panic!("expected text frame");
    };
    serde_json::from_str(&text).unwrap()
}

pub async fn send(socket: &mut Socket, frame: Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

pub async fn no_call(socket: &mut Socket) {
    assert!(
        tokio::time::timeout(Duration::from_millis(250), socket.next())
            .await
            .is_err(),
        "query must not be transmitted or replayed"
    );
}

pub async fn station_call(socket: &mut Socket, id: &str, action: &str, payload: Value) -> Value {
    send(socket, json!([2, id, action, payload])).await;
    let reply = receive(socket).await;
    assert_eq!(reply[0], 3);
    assert_eq!(reply[1], id);
    reply[2].clone()
}

pub async fn boot(socket: &mut Socket) {
    assert_eq!(
        station_call(
            socket,
            "boot",
            "BootNotification",
            json!({"chargePointVendor":"Independent","chargePointModel":"SocketPeer"}),
        )
        .await["status"],
        "Accepted"
    );
}

pub fn command(id: &str, connector: u32, unit: Option<&str>) -> Value {
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-a"});
    if connector > 0 {
        let canonical = if connector == 1 { "one" } else { "two" };
        resource["resource"] = json!({"kind":"connector","connector_id":canonical});
        resource["native_protocol_reference"] =
            json!({"protocol":"ocpp16","connector_id":connector});
    }
    let mut payload = json!({"connectorId":connector,"duration":120});
    if let Some(unit) = unit {
        payload["chargingRateUnit"] = json!(unit);
    }
    json!({
        "request_id":id,"resource":resource,
        "operation":{"kind":"ocpp","parameters":{
            "protocol":"ocpp16j","action":"GetCompositeSchedule",
            "payload_schema":SCHEMA,"payload":payload
        }},
        "expires_at":"2099-01-01T00:00:00Z"
    })
}

pub fn begin(client: &reqwest::Client, fixture: &Fixture, body: Value) -> Pending {
    let client = client.clone();
    let url = fixture.url("/api/v1/commands");
    tokio::spawn(async move {
        client
            .post(url)
            .bearer_auth(PRIVILEGED)
            .json(&body)
            .send()
            .await
            .unwrap()
    })
}

pub async fn completed(request: Pending) -> Value {
    let response = request.await.unwrap();
    assert_eq!(response.status(), 202);
    response.json::<Value>().await.unwrap()["result"].clone()
}

pub async fn result(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    let response = client
        .get(fixture.url(&format!("/api/v1/commands/{id}")))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

pub async fn query(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    reply: Value,
) -> Value {
    let id = body["request_id"].as_str().unwrap().to_owned();
    let request = begin(client, fixture, body.clone());
    let call = receive(socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], "GetCompositeSchedule");
    assert_eq!(call[3], body["operation"]["parameters"]["payload"]);
    send(socket, json!([3, call[1], reply])).await;
    let value = completed(request).await;
    assert_eq!(result(client, fixture, &id).await, value);
    value
}

pub fn accepted(unit: &str) -> Value {
    json!({
        "status":"Accepted","scheduleStart":"2026-10-01T10:00:00Z",
        "chargingSchedule":{
            "chargingRateUnit":unit,"chargingSchedulePeriod":[{"startPeriod":0,"limit":0}]
        }
    })
}
