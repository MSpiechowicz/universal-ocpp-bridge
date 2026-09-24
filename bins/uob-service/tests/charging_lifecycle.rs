#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
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
use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::{
    BridgeId, ContractVersion, EventEnvelope, EventId, EventOrigin, EventType, ResourceRef,
    StationEvent, StationId, TransactionSnapshot, UtcTimestamp,
};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

const READ_TOKEN: &str = "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ALPHA: &str = "c3RhdGlvbi1hOnN0YXRpb24tYWxwaGEtc2VjcmV0LTEyMzQ1";

struct Fixture {
    root: PathBuf,
    configuration: PathBuf,
    management: u16,
    charging: u16,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("uob-charging-lifecycle-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        for (name, value) in [
            ("read-grant", READ_TOKEN),
            ("station-a", "station-alpha-secret-12345"),
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
        let configuration = root.join("bridge.toml");
        fs::write(
            &configuration,
            format!(
                "[bridge]\nid='bridge-1'\nenvironment='demo'\n\
             [management]\nlisten_addr='127.0.0.1:{management}'\n\
             [charging]\nenabled=true\nlisten_addr='127.0.0.1:{charging}'\n\
             state_directory='{}'\nread_grant_file='{}'\n\
             [[charging.stations]]\nid='station-a'\nprotocol='ocpp16j'\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nconnector_id='connector-1'\nnative_connector_id=1\n",
                state.display(),
                root.join("read-grant").display(),
                root.join("station-a").display()
            ),
        )
        .unwrap();
        Self {
            root,
            configuration,
            management,
            charging,
        }
    }
    fn start(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_uob"))
            .args(["serve", "--config"])
            .arg(&self.configuration)
            .arg("--no-ui")
            .env_remove("NOTIFY_SOCKET")
            .env_remove("WATCHDOG_USEC")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    fn state(&self) -> PathBuf {
        self.root.join("state")
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
async fn ready(child: &mut Child, port: u16) {
    for _ in 0..100 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "charging process exited during startup"
        );
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("charging listener did not start");
}
fn request(port: u16) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://127.0.0.1:{port}/ocpp/station-a")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    request
        .headers_mut()
        .insert("Authorization", format!("Basic {ALPHA}").parse().unwrap());
    request
}
async fn next_invalidation(
    response: &mut reqwest::Response,
    pending: &mut String,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(end) = pending.find("\n\n") {
                let frame: String = pending.drain(..end + 2).collect();
                if let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) {
                    return serde_json::from_str(data).expect("valid station SSE event");
                }
                continue;
            }
            let bytes = response.chunk().await.unwrap().expect("live station event");
            pending.push_str(std::str::from_utf8(&bytes).unwrap());
            assert!(pending.len() < 256 * 1024, "bounded SSE frame");
        }
    })
    .await
    .expect("station event arrived")
}

async fn call(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request: serde_json::Value,
) -> serde_json::Value {
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("OCPP response arrived")
        .expect("socket remained open")
        .expect("valid OCPP response");
    let Message::Text(text) = response else {
        panic!("expected OCPP text response");
    };
    serde_json::from_str(&text).unwrap()
}

async fn detail(client: &reqwest::Client, port: u16) -> serde_json::Value {
    client
        .get(format!("http://127.0.0.1:{port}/api/v1/stations/station-a"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn configure_edition(fixture: &Fixture, edition: &str) {
    if edition == "ocpp201" {
        let config = fs::read_to_string(&fixture.configuration)
            .unwrap()
            .replace("protocol='ocpp16j'", "protocol='ocpp201'")
            .replace(
                "connector_id='connector-1'\nnative_connector_id=1",
                "evse_id='evse-1'\nnative_evse_id=1\n\
                 [[charging.stations.resources]]\nevse_id='evse-1'\n\
                 connector_id='connector-1'\nnative_evse_id=1\nnative_connector_id=1",
            );
        fs::write(&fixture.configuration, config).unwrap();
    }
}

fn assert_registration_points(snapshot: &serde_json::Value, prefix: &str) {
    let value = |name: &str| {
        snapshot["current_values"]
            .as_array()
            .unwrap()
            .iter()
            .find(|point| point["point_id"] == format!("{prefix}/registration/{name}"))
            .unwrap()["value"]
            .clone()
    };
    assert_eq!(
        value("status"),
        serde_json::json!({"type":"text","value":"Accepted"})
    );
    assert_eq!(
        value("vendor"),
        serde_json::json!({"type":"text","value":"LiveVendor"})
    );
}

#[tokio::test]
async fn live_registration_and_heartbeat_invalidate_scoped_station_detail() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for (edition, protocol, boot, prefix) in [
        (
            "ocpp16j",
            "ocpp1.6",
            serde_json::json!([2, "boot", "BootNotification", {
                "chargePointVendor": "LiveVendor", "chargePointModel": "LiveModel"
            }]),
            "ocpp16",
        ),
        (
            "ocpp201",
            "ocpp2.0.1",
            serde_json::json!([2, "boot", "BootNotification", {
                "chargingStation": {"vendorName": "LiveVendor", "model": "LiveModel"},
                "reason": "PowerUp"
            }]),
            "ocpp201",
        ),
    ] {
        let fixture = Fixture::new();
        configure_edition(&fixture, edition);
        let mut child = fixture.start();
        ready(&mut child, fixture.management).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut stream = client.get(format!(
            "http://127.0.0.1:{}/api/v1/events?station_id=station-a&types=station.snapshot.invalidated",
            fixture.management
        )).bearer_auth(READ_TOKEN).send().await.unwrap();
        assert_eq!(stream.status(), 200);
        let mut pending = String::new();
        let mut websocket_request = request(fixture.charging);
        websocket_request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
        let (mut socket, _) = connect_async(websocket_request).await.unwrap();
        let connected = next_invalidation(&mut stream, &mut pending).await;
        assert_eq!(connected["event_type"], "station.snapshot.invalidated");
        let before_boot = detail(&client, fixture.management).await;
        assert!(before_boot["current_values"].is_null());
        assert_eq!(
            call(
                &mut socket,
                serde_json::json!([2, "early", "Heartbeat", {}])
            )
            .await[0],
            4
        );
        assert_eq!(detail(&client, fixture.management).await, before_boot);

        assert_eq!(call(&mut socket, boot).await[2]["status"], "Accepted");
        let registered = next_invalidation(&mut stream, &mut pending).await;
        assert_eq!(registered["event_type"], "station.snapshot.invalidated");
        assert_eq!(registered["resource"]["station_id"], "station-a");
        assert_eq!(
            registered["payload"],
            serde_json::json!({
                "station_snapshot_invalidated": "station-a"
            })
        );
        assert!(registered["sequence"].as_u64() > connected["sequence"].as_u64());
        let after_boot = detail(&client, fixture.management).await;
        assert_eq!(registered["observed_at"], after_boot["observed_at"]);
        assert_registration_points(&after_boot, prefix);
        let boot_message = after_boot["connectivity"]["last_message_at"].clone();
        assert!(!boot_message.is_null());

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            call(
                &mut socket,
                serde_json::json!([2, "heartbeat", "Heartbeat", {}])
            )
            .await[0],
            3
        );
        let heartbeat = next_invalidation(&mut stream, &mut pending).await;
        assert_eq!(heartbeat["event_type"], "station.snapshot.invalidated");
        assert_eq!(heartbeat["resource"]["station_id"], "station-a");
        assert_eq!(heartbeat["payload"], registered["payload"]);
        assert!(heartbeat["sequence"].as_u64() > registered["sequence"].as_u64());
        let after_heartbeat = detail(&client, fixture.management).await;
        assert_eq!(heartbeat["observed_at"], after_heartbeat["observed_at"]);
        assert_ne!(
            after_heartbeat["connectivity"]["last_message_at"],
            boot_message
        );
        assert_eq!(
            after_heartbeat["current_values"],
            after_boot["current_values"]
        );
        stop(child);
    }
}

#[tokio::test]
async fn live_station_sse_invalidates_on_disconnect_and_reconnect() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let mut child = fixture.start();
    ready(&mut child, fixture.management).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut stream = client.get(format!(
        "http://127.0.0.1:{}/api/v1/events?station_id=station-a&types=station.snapshot.invalidated",
        fixture.management
    )).bearer_auth(READ_TOKEN).send().await.unwrap();
    assert_eq!(stream.status(), 200);
    let mut pending = String::new();
    let (socket, _) = connect_async(request(fixture.charging)).await.unwrap();
    let mut socket = Some(socket);
    for (index, expected) in ["connected", "disconnected", "connected"]
        .into_iter()
        .enumerate()
    {
        if index == 1 {
            drop(socket.take());
        } else if index == 2 {
            socket = Some(connect_async(request(fixture.charging)).await.unwrap().0);
        }
        let event = next_invalidation(&mut stream, &mut pending).await;
        assert_eq!(event["event_type"], "station.snapshot.invalidated");
        assert_eq!(event["resource"]["station_id"], "station-a");
        assert_eq!(
            event["payload"],
            serde_json::json!({"station_snapshot_invalidated":"station-a"})
        );
        let detail: serde_json::Value = client
            .get(format!(
                "http://127.0.0.1:{}/api/v1/stations/station-a",
                fixture.management
            ))
            .bearer_auth(READ_TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(detail["connectivity"]["status"], expected);
        assert_eq!(detail["resources"].as_array().unwrap().len(), 1);
    }
    stop(child);
}

#[tokio::test]
async fn marker_only_first_start_recovers_but_unbound_database_is_rejected() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let marker = state.join("charging.identity");
    fs::write(&marker, b"bridge-1").unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;
    stop(child);
    assert!(state.join("charging.sqlite3").exists());
    fs::remove_file(&marker).unwrap();
    assert!(!fixture.start().wait().unwrap().success());
    assert!(
        !marker.exists(),
        "an existing unbound DB must not be adopted"
    );
    fs::write(&marker, b"other-bridge").unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!fixture.start().wait().unwrap().success());
}

#[tokio::test]
async fn charging_startup_prunes_expired_journal_on_private_worker() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;
    stop(child);
    let path = fixture.state().join("charging.sqlite3");
    let store: SqliteOperationalStore<
        serde_json::Value,
        StationEvent,
        TransactionSnapshot,
        String,
    > = SqliteOperationalStore::open(&path, 16).unwrap();
    let sequence = store.reserve_event_sequence().await.unwrap();
    let expired_at = UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH);
    let mut write = AtomicStoreWrite::empty();
    write.journal_events.push(EventEnvelope {
        event_id: EventId::new(format!("bridge-1/expired/{sequence}")).unwrap(),
        schema_version: ContractVersion::V1_INITIAL,
        runtime: serde_json::from_value(serde_json::json!({
            "environment":"demo", "release_id":"test", "release_digest":"sha256:test",
            "process_instance_id":"test-process"
        }))
        .unwrap(),
        resource: ResourceRef {
            bridge_id: BridgeId::new("bridge-1").unwrap(),
            station_id: StationId::new("station-a").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        source_time: None,
        observed_at: expired_at,
        event_type: EventType::new("station.snapshot.invalidated").unwrap(),
        origin: EventOrigin::Bridge,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::Invalidation {
            station_snapshot_invalidated: StationId::new("station-a").unwrap(),
        },
    });
    store.write_atomic(write).await.unwrap();
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(store);
    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;
    stop(child);
    let store: SqliteOperationalStore<
        serde_json::Value,
        StationEvent,
        TransactionSnapshot,
        String,
    > = SqliteOperationalStore::open(&path, 16).unwrap();
    let status = store.storage_retention_status().await.unwrap();
    assert_eq!(status.retained_critical_events, 0);
    assert!(status.pruned_expired_events >= 1);
    store.shutdown(Duration::from_secs(2)).await.unwrap();
}
