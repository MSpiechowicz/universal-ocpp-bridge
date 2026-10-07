pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Socket, begin, boot, client, completed, no_call, receive, result,
    send, station_call,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fmt::Write, fs, os::unix::fs::PermissionsExt};

fn vacant_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Station A enables the given log families; station B stays without them. The artifact
/// service runs without a firmware catalog.
pub fn fixture(diagnostics: bool, log: bool, trigger: bool) -> (Fixture, u16) {
    let fixture = Fixture::new();
    let port = vacant_port();
    let spool = fixture.root.join("artifact-spool");
    fs::create_dir(&spool).unwrap();
    fs::set_permissions(&spool, fs::Permissions::from_mode(0o700)).unwrap();
    let path = fixture.root.join("bridge.toml");
    let mut options =
        String::from("diagnostics_job_timeout_seconds=600\ndiagnostics_upload_max_bytes=65536");
    for (enabled, key) in [
        (diagnostics, "get_diagnostics"),
        (log, "get_log"),
        (trigger, "trigger_message"),
    ] {
        if enabled {
            let _ = write!(options, "\n{key}=true");
        }
    }
    let mut config = fs::read_to_string(&path)
        .unwrap()
        .replace("get_composite_schedule=true", &options);
    let _ = write!(
        config,
        "[charging.firmware]\nlisten_addr='127.0.0.1:{port}'\nspool_directory='{}'\n",
        spool.display(),
    );
    fs::write(path, config).unwrap();
    (fixture, port)
}

pub fn station() -> Value {
    json!({"bridge_id":"bridge-1","station_id":"station-a"})
}

pub fn diagnostics_command(id: &str) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":"GetDiagnostics",
        "payload_schema":"urn:uob:ocpp16:GetDiagnosticsReference:1",
        "payload":{"startTime":"2026-01-01T00:00:00Z","stopTime":"2026-01-02T00:00:00Z","retries":1}
    }},"expires_at":"2099-01-01T00:00:00Z"})
}

pub fn log_command(id: &str, log_type: &str, request_id: i32) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":"GetLog",
        "payload_schema":"urn:uob:ocpp16:GetLogReference:1",
        "payload":{"logType":log_type,"requestId":request_id,
            "oldestTimestamp":"2026-01-01T00:00:00Z","retryInterval":5}
    }},"expires_at":"2099-01-01T00:00:00Z"})
}

/// Uploads like a station (N01.FR.21: the reported file name is appended to the directory).
pub async fn upload(location: &str, file_name: &str, bytes: &[u8]) -> u16 {
    assert!(location.ends_with('/'), "{location}");
    client()
        .put(format!("{location}{file_name}"))
        .body(bytes.to_vec())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

pub fn log_bytes(seed: u8) -> Vec<u8> {
    (0..20_011_u32)
        .map(|index| u8::try_from(index % 241).unwrap() ^ seed)
        .collect()
}

/// A refused command returns its durable or synthetic result with a client error status.
pub async fn submit_refused(
    client: &reqwest::Client,
    fixture: &Fixture,
    body: Value,
) -> (u16, Value) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

pub async fn notify(socket: &mut Socket, id: &str, action: &str, payload: Value) {
    assert_eq!(station_call(socket, id, action, payload).await, json!({}));
}

pub async fn job(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    result(client, fixture, id).await["diagnostics_16"]["job"].clone()
}
