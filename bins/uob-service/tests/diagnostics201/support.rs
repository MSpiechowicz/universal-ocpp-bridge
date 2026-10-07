pub use crate::legacy::{
    CONTROL, Fixture, Socket, begin, client, completed, log_bytes, no_call, receive, result, send,
    sha256, station, station_call, submit_refused, upload,
};
use serde_json::{Value, json};
use std::fs;

/// Station A becomes an OCPP 2.0.1 station with `GetLog`; optionally with `TriggerMessage`.
/// Station B stays without it. The artifact service runs without a firmware catalog.
pub fn fixture(log: bool, trigger: bool) -> (Fixture, u16) {
    let (fixture, port) = crate::legacy::fixture(false, log, trigger);
    ocpp201(&fixture);
    (fixture, port)
}

/// A 2.0.1 station A with no log opt-in and, as nothing would use it, no artifact service.
pub fn fixture_without_logs() -> Fixture {
    let fixture = Fixture::new();
    ocpp201(&fixture);
    fixture
}

fn ocpp201(fixture: &Fixture) {
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
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
}

pub fn command(id: &str, log_type: &str, request_id: i32) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp201","action":"GetLog",
        "payload_schema":"urn:uob:ocpp201:GetLogReference:1",
        "payload":{"logType":log_type,"requestId":request_id,
            "oldestTimestamp":"2026-01-01T00:00:00Z","latestTimestamp":"2026-01-02T00:00:00Z",
            "retries":2,"retryInterval":5}
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
        station_call(socket, id, "LogStatusNotification", payload).await,
        json!({})
    );
}

pub async fn job(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    result(client, fixture, id).await["diagnostics_201"]["job"].clone()
}

/// Sends the request, answers with `reply` and returns the native call and the public result.
pub async fn answered(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    reply: Value,
) -> (Value, Value) {
    let submission = begin(client, fixture, body);
    let call = receive(socket).await;
    assert_eq!(call[2], "GetLog");
    send(socket, json!([3, call[1], reply])).await;
    (call, completed(submission).await)
}

/// The upload location of a native `GetLogRequest`.
pub fn location(call: &Value) -> String {
    call[3]["log"]["remoteLocation"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub fn assert_private(value: &Value) {
    for marker in ["/uploads/", "http://", "remoteLocation", "upload_id"] {
        assert!(!value.to_string().contains(marker), "{marker}");
    }
}
