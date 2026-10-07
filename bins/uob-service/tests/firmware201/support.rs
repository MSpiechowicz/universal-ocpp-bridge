pub use crate::firmware::{
    CONTROL, Fixture, SIGNED_IMAGE, Socket, begin, client, completed, download, image, no_call,
    receive, result, send, station_accepts, station_call, submit_refused,
};
use serde_json::{Value, json};
use std::fs;
pub const PLAIN_IMAGE: &str = crate::firmware::LEGACY_IMAGE;

/// Station A becomes an OCPP 2.0.1 station with secure (L01) or non-secure (L02) firmware.
pub fn fixture(secure: bool, trigger: bool) -> (Fixture, u16) {
    let (fixture, port) = crate::firmware::fixture(false, trigger);
    let path = fixture.root.join("bridge.toml");
    let mode = if secure {
        "update_firmware=true"
    } else {
        "update_firmware=true\nnon_secure_firmware=true"
    };
    let config = fs::read_to_string(&path)
        .unwrap()
        .replacen("update_firmware=true", mode, 1)
        .replacen("protocol='ocpp16j'", "protocol='ocpp201'", 1)
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
    (fixture, port)
}

pub fn station() -> Value {
    json!({"bridge_id":"bridge-1","station_id":"station-a"})
}

pub fn command(id: &str, request_id: i32, reference: &str) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp201","action":"UpdateFirmware",
        "payload_schema":"urn:uob:ocpp201:UpdateFirmwareReference:1",
        "payload":{"requestId":request_id,"artifactReference":reference,
            "retrieveDateTime":"2026-01-01T00:00:00Z","installDateTime":"2026-01-01T00:05:00Z",
            "retries":2,"retryInterval":30}
    }},"expires_at":"2099-01-01T00:00:00Z"})
}

pub async fn boot(socket: &mut Socket) {
    send(socket, json!([2,"boot","BootNotification",{"chargingStation":{"model":"model","vendorName":"vendor"},"reason":"PowerUp"}])).await;
    assert_eq!(receive(socket).await[2]["status"], "Accepted");
}

pub async fn connected(fixture: &Fixture, client: &reqwest::Client) -> Socket {
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(client, "station-a").await;
    socket
}

pub async fn notify(socket: &mut Socket, id: &str, payload: Value) {
    assert_eq!(
        station_call(socket, id, "FirmwareStatusNotification", payload).await,
        json!({})
    );
}

pub async fn job_state(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    result(client, fixture, id).await["firmware_201"]["job"]["state"].clone()
}

/// Sends the request, answers with `reply` and returns the completed public result.
pub async fn answered(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    reply: Value,
) -> (Value, Value) {
    let submission = begin(client, fixture, body);
    let call = receive(socket).await;
    assert_eq!(call[2], "UpdateFirmware");
    send(socket, json!([3, call[1], reply])).await;
    (call, completed(submission).await)
}

pub fn assert_private(value: &Value) {
    for marker in [
        "CERTIFICATE",
        "signingCertificate",
        "\"signature\"",
        "http://",
    ] {
        assert!(!value.to_string().contains(marker), "{marker}");
    }
}
