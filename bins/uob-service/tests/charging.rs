#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uob_application::OperationalStore;
use uob_contracts::{BridgeId, ResourceRef, StationEvent, StationId, TransactionSnapshot};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    configuration: PathBuf,
    management: u16,
    charging: u16,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("uob-charging-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        for (name, value) in [
            ("read-grant", "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
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
        let configuration = root.join("bridge.toml");
        let document = format!(
            "[bridge]\nid='bridge-1'\nenvironment='demo'\n\
             [management]\nlisten_addr='127.0.0.1:{management}'\n\
             [charging]\nenabled=true\nlisten_addr='127.0.0.1:{charging}'\n\
             state_directory='{}'\nread_grant_file='{}'\n\
             [[charging.stations]]\nid='station-a'\nprotocol='ocpp16j'\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nconnector_id='connector-1'\nnative_connector_id=1\n\
             [[charging.stations]]\nid='station-b'\nprotocol='ocpp201'\ncredential_file='{}'\n\
             [[charging.stations.resources]]\nevse_id='evse-1'\nnative_evse_id=1\n\
             [[charging.stations.resources]]\nevse_id='evse-1'\nconnector_id='connector-1'\n\
             native_evse_id=1\nnative_connector_id=1\n",
            state.display(),
            root.join("read-grant").display(),
            root.join("station-a").display(),
            root.join("station-b").display(),
        );
        fs::write(&configuration, document).unwrap();
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
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
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
fn request(
    port: u16,
    station: &str,
    protocol: &str,
    authorization: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://127.0.0.1:{port}/ocpp/{station}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
    request.headers_mut().insert(
        "Authorization",
        format!("Basic {authorization}").parse().unwrap(),
    );
    request
}
async fn call(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    message: serde_json::Value,
) -> serde_json::Value {
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(message.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = response else {
        panic!("expected OCPP text response");
    };
    serde_json::from_str(&text).unwrap()
}
fn station(name: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-1").unwrap(),
        station_id: StationId::new(name).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}

const READ_TOKEN: &str = "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn verify_management_access(client: &reqwest::Client, management: u16) {
    let url = |path: &str| format!("http://127.0.0.1:{management}{path}");
    for path in [
        "/api/v1/stations",
        "/api/v1/stations/station-a",
        "/api/v1/events",
    ] {
        assert_eq!(client.get(url(path)).send().await.unwrap().status(), 401);
        assert_eq!(
            client
                .get(url(path))
                .bearer_auth("uob1.demo.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .get(url(path))
                .bearer_auth("uob1.production.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    for path in [
        "/api/v1/stations/unknown",
        "/api/v1/events?station_id=unknown",
    ] {
        assert_eq!(
            client
                .get(url(path))
                .bearer_auth(READ_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let inventory = client
        .get(url("/api/v1/stations?limit=1"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(inventory.status(), 200);
    let page: serde_json::Value = inventory.json().await.unwrap();
    assert!(
        page["items"].as_array().unwrap().is_empty(),
        "never invent station snapshots"
    );
}

async fn next_station_event(
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

async fn verify_retained_stream(client: &reqwest::Client, management: u16) {
    let mut response = client
        .get(format!("http://127.0.0.1:{management}/api/v1/events"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut pending = String::new();
    loop {
        let event = next_station_event(&mut response, &mut pending).await;
        if event["event_type"] == "station.availability.observed" {
            assert_eq!(event["resource"]["station_id"], "station-a");
            break;
        }
    }
}

#[tokio::test]
async fn daemon_management_reads_and_events_require_the_station_scoped_demo_bearer() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let mut child = fixture.start();
    ready(&mut child, fixture.management).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = |path: &str| format!("http://127.0.0.1:{}{path}", fixture.management);
    verify_management_access(&client, fixture.management).await;
    let alpha = "c3RhdGlvbi1hOnN0YXRpb24tYWxwaGEtc2VjcmV0LTEyMzQ1";
    let (mut socket, _) = connect_async(request(fixture.charging, "station-a", "ocpp1.6", alpha))
        .await
        .unwrap();
    assert_eq!(call(&mut socket, serde_json::json!([2,"boot","BootNotification",{"chargePointVendor":"VendorA","chargePointModel":"ModelA"}])).await[2]["status"], "Accepted");
    assert_eq!(call(&mut socket, serde_json::json!([2,"status","StatusNotification",{"connectorId":1,"status":"Available","errorCode":"NoError"}])).await[0], 3);
    let inventory = client
        .get(url("/api/v1/stations"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(inventory.status(), 200);
    let page: serde_json::Value = inventory.json().await.unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["station"]["station_id"], "station-a");
    assert_eq!(items[0]["station"]["bridge_id"], "bridge-1");
    let detail = client
        .get(url("/api/v1/stations/station-a"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(detail.status(), 200);
    let snapshot: serde_json::Value = detail.json().await.unwrap();
    assert_eq!(snapshot["station"], items[0]["station"]);
    let bravo = "c3RhdGlvbi1iOnN0YXRpb24tYnJhdm8tc2VjcmV0LTY3ODkw";
    let (mut other, _) = connect_async(request(fixture.charging, "station-b", "ocpp2.0.1", bravo))
        .await
        .unwrap();
    assert_eq!(call(&mut other, serde_json::json!([2,"other-boot","BootNotification",{"chargingStation":{"vendorName":"VendorB","model":"ModelB"},"reason":"PowerUp"}])).await[2]["status"], "Accepted");
    let first = client
        .get(url("/api/v1/stations?limit=1"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["items"][0]["station"]["station_id"], "station-a");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("another authorized station");
    let second = client
        .get(url("/api/v1/stations"))
        .query(&[("limit", "1"), ("after", cursor)])
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let second: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["items"][0]["station"]["station_id"], "station-b");
    let other_detail = client
        .get(url("/api/v1/stations/station-b"))
        .bearer_auth(READ_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(other_detail.status(), 200);
    assert_eq!(
        other_detail.json::<serde_json::Value>().await.unwrap()["station"]["station_id"],
        "station-b"
    );
    verify_retained_stream(&client, fixture.management).await;
    drop(other);
    drop(socket);
    stop(child);
}

#[tokio::test]
async fn authenticated_editions_commit_isolated_station_state_and_recover_after_restart() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;
    let alpha = "c3RhdGlvbi1hOnN0YXRpb24tYWxwaGEtc2VjcmV0LTEyMzQ1";
    let bravo = "c3RhdGlvbi1iOnN0YXRpb24tYnJhdm8tc2VjcmV0LTY3ODkw";
    for (name, edition, credential) in [
        ("station-a", "ocpp2.0.1", alpha),
        ("station-b", "ocpp1.6", bravo),
        ("station-a", "ocpp1.6", bravo),
        ("unknown", "ocpp1.6", alpha),
    ] {
        assert!(
            connect_async(request(fixture.charging, name, edition, credential))
                .await
                .is_err()
        );
    }
    let (mut a, _) = connect_async(request(fixture.charging, "station-a", "ocpp1.6", alpha))
        .await
        .unwrap();
    assert!(
        connect_async(request(fixture.charging, "station-a", "ocpp1.6", alpha))
            .await
            .is_err()
    );
    let (mut b, _) = connect_async(request(fixture.charging, "station-b", "ocpp2.0.1", bravo))
        .await
        .unwrap();
    assert_eq!(call(&mut a, serde_json::json!([2,"a-boot","BootNotification",{"chargePointVendor":"VendorA","chargePointModel":"ModelA"}])).await[2]["status"], "Accepted");
    assert_eq!(call(&mut b, serde_json::json!([2,"b-boot","BootNotification",{"chargingStation":{"vendorName":"VendorB","model":"ModelB"},"reason":"PowerUp"}])).await[2]["status"], "Accepted");
    assert_eq!(call(&mut a, serde_json::json!([2,"a-status","StatusNotification",{"connectorId":1,"status":"Available","errorCode":"NoError"}])).await[0], 3);
    assert_eq!(
        call(
            &mut a,
            serde_json::json!([2,"a-unhandled","DataTransfer",{"vendorId":"demo"}])
        )
        .await[0],
        4
    );
    assert_eq!(call(&mut b, serde_json::json!([2,"b-status","StatusNotification",{"timestamp":"2026-09-24T12:00:00Z","connectorStatus":"Occupied","evseId":1,"connectorId":1}])).await[0], 3);
    let start_a = call(&mut a, serde_json::json!([2,"a-start","StartTransaction",{"connectorId":1,"idTag":"ID-A","meterStart":0,"timestamp":"2026-09-24T12:00:00Z"}])).await;
    assert_eq!(start_a[0], 3);
    assert_eq!(start_a[2]["idTagInfo"]["status"], "Invalid");
    let start_b = call(&mut b, serde_json::json!([2,"b-start","TransactionEvent",{"eventType":"Started","timestamp":"2026-09-24T12:00:00Z","triggerReason":"CablePluggedIn","seqNo":0,"transactionInfo":{"transactionId":"tx-b"},"evse":{"id":1,"connectorId":1}}])).await;
    assert_eq!(start_b[0], 3);
    assert_eq!(start_b[2]["idTokenInfo"]["status"], "Invalid");
    assert_eq!(call(&mut a, serde_json::json!([2,"a-meter","MeterValues",{"connectorId":1,"meterValue":[{"timestamp":"2026-09-24T12:00:01Z","sampledValue":[{"value":"12.5"}]}]}])).await[0], 3);
    assert_eq!(call(&mut b, serde_json::json!([2,"b-meter","MeterValues",{"evseId":1,"meterValue":[{"timestamp":"2026-09-24T12:00:01Z","sampledValue":[{"value":7.5}]}]}])).await[0], 3);
    drop(a);
    drop(b);
    stop(child);
    let path = fixture.path("state").join("charging.sqlite3");
    let store: SqliteOperationalStore<
        serde_json::Value,
        StationEvent,
        TransactionSnapshot,
        String,
    > = SqliteOperationalStore::open(path, 16).unwrap();
    let a = store
        .station_snapshot(station("station-a"))
        .await
        .unwrap()
        .unwrap();
    let b = store
        .station_snapshot(station("station-b"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.resources[0].resource.station_id.as_str(), "station-a");
    assert_eq!(b.resources[0].resource.station_id.as_str(), "station-b");
    assert!(a.transactions.iter().any(|tx| tx.ocpp16.is_some()));
    assert!(b.transactions.iter().any(|tx| {
        tx.protocol_state
            .as_ref()
            .is_some_and(|state| state.native_transaction_id == "tx-b")
    }));
    assert_ne!(a.resources[0].resource, b.resources[0].resource);
    assert!(!a.resources[0].current_values.is_empty());
    assert!(!b.resources[0].current_values.is_empty());
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    let mut child = fixture.start();
    ready(&mut child, fixture.charging).await;
    let (mut a, _) = connect_async(request(fixture.charging, "station-a", "ocpp1.6", alpha))
        .await
        .unwrap();
    assert_eq!(
        call(
            &mut a,
            serde_json::json!([2, "before-reboot", "Heartbeat", {}])
        )
        .await[0],
        4
    );
    assert_eq!(call(&mut a, serde_json::json!([2,"after-reboot","BootNotification",{"chargePointVendor":"VendorA","chargePointModel":"ModelA"}])).await[0], 3);
    stop(child);
}

#[test]
fn symlinked_or_world_readable_credential_fails_startup() {
    let fixture = Fixture::new();
    let credential = fixture.path("station-a");
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();
    let status = fixture.start().wait().unwrap();
    assert!(!status.success());
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    let backup = fixture.path("station-a-backup");
    fs::rename(&credential, &backup).unwrap();
    symlink(&backup, &credential).unwrap();
    assert!(!fixture.start().wait().unwrap().success());
}
