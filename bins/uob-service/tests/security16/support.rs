pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Process, READ, Socket, begin, boot, client, completed, no_call,
    receive, result, send, station_call,
};
use serde_json::{Value, json};
use std::{fmt::Write, fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

/// Secrets of `security-event-tech-info-credential.json`: the password and the base64 pair of a
/// Basic header. Neither may appear anywhere the daemon persists or publishes.
pub const SECRETS: [&str; 2] = ["not-a-real-password-0000", "ZmFrZXVzZXI6ZmFrZXBhc3N3b3Jk"];
pub const SCHEMA: &str = "urn:OCPP:Cp:1.6:2020:3:ExtendedTriggerMessage.req";
const EVENTS: &str = "/api/v1/events?station_id=station-a&types=station.security_event.16";

/// Daemons are started one at a time: ports are picked before they are bound, and starting many
/// daemons at once makes a port collision likely enough to flake a whole run.
static STARTUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Which optional station A features the fixture enables.
#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)] // Each flag is one independent option line.
pub struct Options {
    pub extended: bool,
    pub plain_trigger: bool,
    pub log: bool,
    pub signed: bool,
}

pub async fn launch(options: Options) -> (Fixture, Process) {
    let _startup = STARTUP.lock().await;
    let fixture = fixture(options);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    (fixture, process)
}

/// Starts the daemon again on the same state after the previous one was dropped.
pub async fn restart(fixture: &Fixture) -> Process {
    let _startup = STARTUP.lock().await;
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    process
}

fn vacant_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// Station A gets exactly the requested options; station B stays an OCPP 1.6 station without any.
fn fixture(options: Options) -> Fixture {
    let fixture = Fixture::new();
    let mut lines = String::new();
    for (enabled, line) in [
        (options.extended, "extended_trigger_message=true"),
        (options.plain_trigger, "trigger_message=true"),
        (
            options.log,
            "get_log=true\ndiagnostics_job_timeout_seconds=600",
        ),
        (
            options.signed,
            "signed_update_firmware=true\nfirmware_job_timeout_seconds=600",
        ),
    ] {
        if enabled {
            let _ = writeln!(lines, "{line}");
        }
    }
    let path = fixture.root.join("bridge.toml");
    let mut config = fs::read_to_string(&path)
        .unwrap()
        .replace("get_composite_schedule=true", lines.trim_end());
    if options.log || options.signed {
        let spool = fixture.root.join("artifact-spool");
        fs::create_dir(&spool).unwrap();
        fs::set_permissions(&spool, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = write!(
            config,
            "[charging.firmware]\nlisten_addr='127.0.0.1:{}'\nspool_directory='{}'\n",
            vacant_port(),
            spool.display(),
        );
        if options.signed {
            private(&fixture.root.join("signed.bin"), &[7_u8; 4096]);
            let catalog = json!({"artifacts":[{"reference":"station-fw-2.0-signed.bin",
                "file":fixture.root.join("signed.bin"),"signed":true}]});
            private(
                &fixture.root.join("catalog.json"),
                catalog.to_string().as_bytes(),
            );
            let _ = writeln!(
                config,
                "catalog_file='{}'",
                fixture.root.join("catalog.json").display()
            );
        }
    }
    fs::write(path, config).unwrap();
    fixture
}

/// Station A as an OCPP 2.0.1 station without options; station B stays OCPP 1.6.
pub async fn launch_201() -> (Fixture, Process) {
    let _startup = STARTUP.lock().await;
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replacen("protocol='ocpp16j'", "protocol='ocpp201'", 1)
        .replacen("get_composite_schedule=true", "", 1)
        .replacen(
            "connector_id='one'\nnative_connector_id=1",
            "evse_id='one'\nnative_evse_id=1",
            1,
        )
        .replacen(
            "connector_id='two'\nnative_connector_id=2",
            "evse_id='two'\nnative_evse_id=2",
            1,
        );
    fs::write(path, config).unwrap();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    (fixture, process)
}

pub async fn connected(fixture: &Fixture, client: &reqwest::Client) -> Socket {
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(client, "station-a").await;
    socket
}

/// The corpus wire frame of one pinned fixture, with its native message id.
pub fn wire(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/1.6")
        .join(name);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// One native `SecurityEventNotification` CALL.
pub fn event_call(id: &str, kind: &str, timestamp: &str, tech_info: Option<&str>) -> Value {
    let mut payload = json!({"type":kind,"timestamp":timestamp});
    if let Some(text) = tech_info {
        payload["techInfo"] = json!(text);
    }
    json!([2, id, "SecurityEventNotification", payload])
}

/// Sends a frame and returns the station-visible reply, whatever it is.
pub async fn exchange(socket: &mut Socket, frame: Value) -> Value {
    send(socket, frame).await;
    receive(socket).await
}

/// Sends a CALL and expects the empty native response.
pub async fn notify(socket: &mut Socket, frame: Value) {
    let id = frame[1].clone();
    assert_eq!(exchange(socket, frame).await, json!([3, id, {}]));
}

/// Server-sent journal events of the security-event type for station A.
pub struct Journal {
    response: reqwest::Response,
    pending: String,
    /// Everything the stream delivered, for secret scans.
    pub raw: String,
}

impl Journal {
    pub async fn open(client: &reqwest::Client, fixture: &Fixture) -> Self {
        let response = client
            .get(fixture.url(EVENTS))
            .bearer_auth(READ)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        Self {
            response,
            pending: String::new(),
            raw: String::new(),
        }
    }

    pub async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(end) = self.pending.find("\n\n") {
                    let frame: String = self.pending.drain(..end + 2).collect();
                    if let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) {
                        return serde_json::from_str(data).expect("valid journal event");
                    }
                    continue;
                }
                let bytes = self.response.chunk().await.unwrap().expect("open stream");
                let text = std::str::from_utf8(&bytes).unwrap();
                self.pending.push_str(text);
                self.raw.push_str(text);
                assert!(self.pending.len() < 1024 * 1024, "bounded frame");
            }
        })
        .await
        .expect("journal event arrives")
    }

    /// Whether another event arrives within a short grace period.
    pub async fn quiet(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(400), self.next())
            .await
            .is_err()
    }
}

/// How many security-event records the journal replays right now.
pub async fn stored(client: &reqwest::Client, fixture: &Fixture) -> usize {
    let mut journal = Journal::open(client, fixture).await;
    let mut count = 0;
    while !journal.quiet().await {
        count += 1;
    }
    count
}

pub fn record(event: &Value) -> &Value {
    &event["payload"]["security_event_16"]
}

/// The bytes of every file in the daemon's private state directory (database, WAL, SHM).
pub fn state_bytes(fixture: &Fixture) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in fs::read_dir(fixture.root.join("state")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            bytes.extend(fs::read(path).unwrap());
        }
    }
    assert!(!bytes.is_empty(), "the state directory holds the database");
    bytes
}

pub fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The daemon's runtime counter of diagnostics dropped under resource pressure.
pub async fn dropped_diagnostics(client: &reqwest::Client, fixture: &Fixture) -> u64 {
    let body = client
        .get(fixture.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    body.lines()
        .find_map(|line| line.strip_prefix("uob_dropped_diagnostics_total "))
        .expect("dropped diagnostics metric")
        .trim()
        .parse()
        .unwrap()
}

/// A privileged `ExtendedTriggerMessage` command for station A.
pub fn extended_command(id: &str, requested: &str, connector: Option<u32>) -> Value {
    let mut payload = json!({"requestedMessage":requested});
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-a"});
    if let Some(connector) = connector {
        payload["connectorId"] = json!(connector);
        if connector > 0 && matches!(requested, "MeterValues" | "StatusNotification") {
            resource["resource"] = json!({"kind":"connector","connector_id":"one"});
            resource["native_protocol_reference"] =
                json!({"protocol":"ocpp16","connector_id":connector});
        }
    }
    json!({"request_id":id,"resource":resource,
        "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j",
            "action":"ExtendedTriggerMessage","payload_schema":SCHEMA,"payload":payload}},
        "expires_at":"2099-01-01T00:00:00Z"})
}

/// A command the management API refuses with the given credential; status and body are returned.
pub async fn refused(
    client: &reqwest::Client,
    fixture: &Fixture,
    token: &str,
    body: &Value,
) -> (u16, Value) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// Submits `body`, expects the native CALL, answers it and returns the call and the HTTP result.
pub async fn answered(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    reply: Value,
) -> (Value, Value) {
    let submission = begin(client, fixture, body);
    let call = receive(socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], "ExtendedTriggerMessage");
    send(socket, json!([3, call[1], reply])).await;
    (call, completed(submission).await)
}

/// Polls a command result until its trigger observation reaches `expected`.
pub async fn observation(
    client: &reqwest::Client,
    fixture: &Fixture,
    id: &str,
    expected: &str,
    limit: Duration,
) -> Value {
    tokio::time::timeout(limit, async {
        loop {
            let value = result(client, fixture, id).await;
            if value["trigger_observation"]["status"] == expected {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("trigger {id} did not reach {expected}"))
}
