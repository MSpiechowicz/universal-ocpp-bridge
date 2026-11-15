//! Shared drivers for the security station tests: configuration, in-memory models and private
//! directories.
#![allow(dead_code)]
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use time::OffsetDateTime;
use uob_sim::security16::{SecurityConfig, SecurityEvent, SecurityHandle};

#[must_use]
pub fn config(fields: Value) -> SecurityConfig {
    let mut base = json!({"private_state_file": "/unused/security.json"});
    let Value::Object(fields) = fields else {
        panic!("configuration fields")
    };
    base.as_object_mut().unwrap().extend(fields);
    serde_json::from_value(base).unwrap()
}

#[must_use]
pub fn station(fields: Value) -> SecurityHandle {
    SecurityHandle::in_memory("alpha", &[1, 2], &config(fields)).unwrap()
}

#[must_use]
pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Claim and acknowledge every queued event, returning their native payloads in delivery order.
pub fn drain(handle: &SecurityHandle) -> Vec<Value> {
    let mut sent = Vec::new();
    while let Some(event) = handle.next_event(now()) {
        handle
            .event_finished(&event, uob_sim::security16::Delivery::Acknowledged, now())
            .unwrap();
        sent.push(event.payload());
    }
    sent
}

#[must_use]
pub fn payload(event: &SecurityEvent) -> Value {
    event.payload()
}

/// An owner-only canonical directory, as the durable native models require.
#[must_use]
pub fn private_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("uob-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::canonicalize(directory).unwrap()
}

pub type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// A reconnecting station connected to `listener` with a `security16` table. `security` holds
/// further keys and sub-tables of that table, `tables` further station tables. Sends no Boot.
/// The WebSocket handshake needs the peer, so `accepted` (the peer's accept of `listener`) runs
/// concurrently and its socket is returned with the station.
pub async fn connect(
    listener: &tokio::net::TcpListener,
    directory: &std::path::Path,
    security: &str,
    tables: &str,
    accepted: impl std::future::Future<Output = Socket>,
) -> (uob_sim::SimulatorProtocolClient, Socket) {
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='1.6'\nrequest_timeout_ms=1000\nreconnect=true\nconnectors=[1]\n[stations.security16]\nprivate_state_file='{}'\n{security}\n{tables}\n",
        listener.local_addr().unwrap(),
        directory.join("security.json").display(),
    );
    let configuration = uob_sim::scenario::parse_configuration(&source).unwrap();
    let (client, socket) = tokio::join!(
        uob_sim::SimulatorProtocolClient::connect(configuration.stations[0].client_config()),
        accepted
    );
    (client.unwrap(), socket)
}

/// A native Boot through the station library; the peer must answer it.
pub async fn boot_call(client: &uob_sim::SimulatorProtocolClient) -> Value {
    use uob_sim::ProtocolClient;
    client
        .call(uob_sim::SimulatorCall {
            action: uob_sim::SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Security"}),
        })
        .await
        .unwrap()
}
