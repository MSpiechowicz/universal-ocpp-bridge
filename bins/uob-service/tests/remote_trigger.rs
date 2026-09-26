#![cfg(unix)]
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

#[path = "remote_trigger/disconnected.rs"]
mod disconnected;
#[path = "remote_trigger/native.rs"]
mod native;

const READ: &str = "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTROL: &str = "uob1.demo.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PRIVILEGED: &str = "uob1.demo.cccccccccccccccccccccccccccccccc";
const SCHEMA: &str = "urn:OCPP:1.6:2019:12:TriggerMessageRequest";
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Fixture {
    root: PathBuf,
    management: u16,
    charging: u16,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("uob-trigger-{}", Uuid::new_v4()));
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
             [[charging.stations]]\nid='station-a'\nprotocol='ocpp16j'\ntrigger_message=true\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nconnector_id='one'\nnative_connector_id=1\n\
             [[charging.stations.resources]]\nconnector_id='two'\nnative_connector_id=2\n\
             [[charging.stations]]\nid='station-b'\nprotocol='ocpp16j'\ntrigger_message=true\ncredential_file='{}'\n\
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
    fn start(&self) -> Child {
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
    async fn ready(&self, child: &mut Child) {
        for _ in 0..100 {
            assert!(
                child.try_wait().unwrap().is_none(),
                "service exited on startup"
            );
            if tokio::net::TcpStream::connect(("127.0.0.1", self.management))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("management listener did not start");
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.management)
    }
    async fn station(&self, name: &str) -> Socket {
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
            .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
        request
            .headers_mut()
            .insert("Authorization", format!("Basic {token}").parse().unwrap());
        connect_async(request).await.unwrap().0
    }
    async fn connected(&self, client: &reqwest::Client, name: &str) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let response = client
                    .get(self.url(&format!("/api/v1/stations/{name}")))
                    .bearer_auth(READ)
                    .send()
                    .await
                    .unwrap();
                assert!(response.status() == 200 || response.status() == 404);
                if response.status() == 200
                    && response.json::<Value>().await.unwrap()["connectivity"]["status"]
                        == "connected"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("authenticated station snapshot is connected");
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
fn stop(mut child: Child) {
    let _ = child.kill();
    child.wait().unwrap();
}
async fn receive(socket: &mut Socket) -> Value {
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
async fn send(socket: &mut Socket, frame: Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}
async fn station_call(socket: &mut Socket, id: &str, class: &str, payload: Value) -> Value {
    send(socket, json!([2, id, class, payload])).await;
    let reply = receive(socket).await;
    assert_eq!(reply[1], id);
    reply
}
fn command(id: &str, requested: &str, connector: Option<u32>) -> Value {
    let mut payload = json!({"requestedMessage":requested});
    if let Some(connector) = connector {
        payload["connectorId"] = json!(connector);
    }
    let resource = if connector.is_some_and(|id| id > 0)
        && matches!(requested, "StatusNotification" | "MeterValues")
    {
        json!({
            "bridge_id":"bridge-1", "station_id":"station-a",
            "resource":{"kind":"connector","connector_id":"one"},
            "native_protocol_reference":{"protocol":"ocpp16","connector_id":1}
        })
    } else {
        json!({"bridge_id":"bridge-1","station_id":"station-a"})
    };
    json!({
        "request_id":id, "resource":resource,
        "operation":{"kind":"ocpp","parameters":{
            "protocol":"ocpp16j","action":"TriggerMessage",
            "payload_schema":SCHEMA,"payload":payload
        }},
        "expires_at":"2099-01-01T00:00:00Z"
    })
}
async fn result(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    let response = client
        .get(fixture.url(&format!("/api/v1/commands/{id}")))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
async fn observation(
    client: &reqwest::Client,
    fixture: &Fixture,
    id: &str,
    expected: &str,
) -> Value {
    for _ in 0..120 {
        let value = result(client, fixture, id).await;
        if value["trigger_observation"]["status"] == expected {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("trigger {id} did not reach {expected}");
}
async fn submit(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    id: &str,
    class: &str,
    connector: Option<u32>,
    response: &str,
) -> Value {
    let url = fixture.url("/api/v1/commands");
    let request_client = client.clone();
    let body = command(id, class, connector);
    let request = tokio::spawn(async move {
        request_client
            .post(url)
            .bearer_auth(PRIVILEGED)
            .json(&body)
            .send()
            .await
            .unwrap()
    });
    let call = receive(socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], "TriggerMessage");
    assert_eq!(call[3]["requestedMessage"], class);
    if let Some(connector) = connector {
        assert_eq!(call[3]["connectorId"], connector);
    }
    send(socket, json!([3, call[1], {"status":response}])).await;
    let reply = request.await.unwrap();
    let status = reply.status();
    if status != 202 {
        let error = reply.text().await.unwrap();
        let lookup = match client
            .get(fixture.url(&format!("/api/v1/commands/{id}")))
            .bearer_auth(READ)
            .send()
            .await
        {
            Ok(reply) => {
                let status = reply.status();
                let body = reply
                    .text()
                    .await
                    .unwrap_or_else(|err| format!("<body read failed: {err}>"));
                format!("{status}: {body}")
            }
            Err(err) => format!("GET failed: {err}"),
        };
        let mut diagnostic = format!(
            "trigger request {id} returned {status}, expected 202; response body: {error}; GET result: {lookup}"
        );
        for credential in [READ, CONTROL, PRIVILEGED] {
            diagnostic = diagnostic.replace(credential, "[redacted]");
        }
        panic!("{diagnostic}");
    }
    let accepted: Value = reply.json().await.unwrap();
    assert_eq!(
        accepted["result"]["trigger_observation"]["native_response"],
        response
    );
    accepted["result"].clone()
}
