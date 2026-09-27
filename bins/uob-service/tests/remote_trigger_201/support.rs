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

pub(super) const READ: &str = "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const CONTROL: &str = "uob1.demo.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PRIVILEGED: &str = "uob1.demo.cccccccccccccccccccccccccccccccc";
const SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:TriggerMessageRequest";
pub(super) type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    management: u16,
    charging: u16,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!("uob-trigger201-{}", Uuid::new_v4()));
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
        ] {
            let path = root.join(name);
            fs::write(&path, value).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let management = vacant_port();
        let charging = loop {
            let port = vacant_port();
            if port != management {
                break port;
            }
        };
        let config = format!(
            "[bridge]\nid='bridge-1'\nenvironment='demo'\n\
             [management]\nlisten_addr='127.0.0.1:{management}'\n\
             [charging]\nenabled=true\nlisten_addr='127.0.0.1:{charging}'\n\
             state_directory='{}'\nread_grant_file='{}'\ncontrol_grant_file='{}'\nprivileged_grant_file='{}'\n\
             [[charging.stations]]\nid='station-a'\nprotocol='ocpp201'\ntrigger_message=true\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nevse_id='one'\nnative_evse_id=1\n\
             [[charging.stations.resources]]\nevse_id='one'\nconnector_id='first'\nnative_evse_id=1\nnative_connector_id=1\n\
             [[charging.stations.resources]]\nevse_id='one'\nconnector_id='second'\nnative_evse_id=1\nnative_connector_id=2\n\
             [[charging.stations.resources]]\nevse_id='two'\nnative_evse_id=2\n\
             [[charging.stations.resources]]\nevse_id='two'\nconnector_id='first'\nnative_evse_id=2\nnative_connector_id=1\n",
            state.display(),
            root.join("read").display(),
            root.join("control").display(),
            root.join("privileged").display(),
            root.join("station-a").display(),
        );
        fs::write(root.join("bridge.toml"), config).unwrap();
        Self {
            root,
            management,
            charging,
        }
    }
    pub(super) fn start(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_uob"))
            .args(["serve", "--config"])
            .arg(self.root.join("bridge.toml"))
            .arg("--no-ui")
            .env_remove("NOTIFY_SOCKET")
            .env_remove("WATCHDOG_USEC")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    pub(super) async fn ready(&self, child: &mut Child) {
        for _ in 0..100 {
            assert!(
                child.try_wait().unwrap().is_none(),
                "service exited before ready"
            );
            if tokio::net::TcpStream::connect(("127.0.0.1", self.management))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("service did not start");
    }
    pub(super) fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.management)
    }
    pub(super) async fn station(&self) -> Socket {
        let mut request = format!("ws://127.0.0.1:{}/ocpp/station-a", self.charging)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp2.0.1".parse().unwrap());
        request.headers_mut().insert(
            "Authorization",
            "Basic c3RhdGlvbi1hOnN0YXRpb24tYWxwaGEtc2VjcmV0LTEyMzQ1"
                .parse()
                .unwrap(),
        );
        connect_async(request).await.unwrap().0
    }
    pub(super) async fn connected(&self, client: &reqwest::Client) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let reply = client
                    .get(self.url("/api/v1/stations/station-a"))
                    .bearer_auth(READ)
                    .send()
                    .await
                    .unwrap();
                if reply.status() == 200
                    && reply.json::<Value>().await.unwrap()["connectivity"]["status"] == "connected"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("station connected");
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
pub(super) fn stop(mut child: Child) {
    let _ = child.kill();
    child.wait().unwrap();
}
async fn send(socket: &mut Socket, frame: Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}
async fn receive(socket: &mut Socket) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(8), socket.next())
        .await
        .unwrap()
        .expect("connected station")
        .expect("valid frame");
    let Message::Text(text) = frame else {
        panic!("expected text frame")
    };
    serde_json::from_str(&text).unwrap()
}
pub(super) async fn station_call(
    socket: &mut Socket,
    id: &str,
    action: &str,
    payload: Value,
) -> Value {
    send(socket, json!([2, id, action, payload])).await;
    let reply = receive(socket).await;
    assert_eq!(reply[1], id);
    reply
}
pub(super) fn command(id: &str, class: &str, evse: Option<(u32, Option<u32>)>) -> Value {
    let mut payload = json!({"requestedMessage": class});
    let mut resource = json!({"bridge_id":"bridge-1", "station_id":"station-a"});
    if let Some((evse_id, connector_id)) = evse {
        let canonical = if evse_id == 1 { "one" } else { "two" };
        payload["evse"] = json!({"id":evse_id});
        if let Some(connector) = connector_id {
            payload["evse"]["connectorId"] = json!(connector);
        }
        resource["resource"] = json!({"kind":"evse", "evse_id":canonical,
            "connector_id": connector_id.map(|id| if id == 1 { "first" } else { "second" })});
        resource["native_protocol_reference"] = json!({"protocol":"ocpp201",
            "evse_id":evse_id, "connector_id":connector_id});
    }
    json!({
        "request_id":id, "resource":resource,
        "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201",
            "action":"TriggerMessage","payload_schema":SCHEMA,"payload":payload}},
        "expires_at":"2099-01-01T00:00:00Z"
    })
}
pub(super) async fn result(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    let response = client
        .get(fixture.url(&format!("/api/v1/commands/{id}")))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
pub(super) async fn until(
    client: &reqwest::Client,
    fixture: &Fixture,
    id: &str,
    status: &str,
) -> Value {
    for _ in 0..120 {
        let value = result(client, fixture, id).await;
        if value["trigger_observation_201"]["status"] == status {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("trigger {id} did not reach {status}");
}
pub(super) async fn submit(
    client: &reqwest::Client,
    fixture: &Fixture,
    station: &mut Socket,
    id: &str,
    class: &str,
    evse: Option<(u32, Option<u32>)>,
    response: Value,
) -> Value {
    let url = fixture.url("/api/v1/commands");
    let request = command(id, class, evse);
    let client = client.clone();
    let task = tokio::spawn(async move {
        client
            .post(url)
            .bearer_auth(PRIVILEGED)
            .json(&request)
            .send()
            .await
            .unwrap()
    });
    let call = receive(station).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], "TriggerMessage");
    assert_eq!(call[3]["requestedMessage"], class);
    send(station, json!([3, call[1], response])).await;
    let reply = task.await.unwrap();
    assert_eq!(reply.status(), 202);
    reply.json::<Value>().await.unwrap()["result"].clone()
}
