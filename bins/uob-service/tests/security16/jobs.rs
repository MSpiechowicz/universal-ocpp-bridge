//! The Whitepaper status messages keep reconciling their durable jobs and now also count as
//! evidence for the extended trigger that asked for them.
use super::support::*;
use serde_json::{Value, json};
use std::time::Duration;

const FULL: Options = Options {
    extended: true,
    plain_trigger: false,
    log: true,
    signed: true,
};

fn station() -> Value {
    json!({"bridge_id":"bridge-1","station_id":"station-a"})
}

fn command(id: &str, action: &str, schema: &str, payload: &Value) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":action,"payload_schema":schema,"payload":payload}},
        "expires_at":"2099-01-01T00:00:00Z"})
}

/// Sends a job command, answers its native CALL with `reply` and returns once it completed.
async fn start(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    reply: Value,
) {
    let submission = begin(client, fixture, body);
    let call = receive(socket).await;
    send(socket, json!([3, call[1], reply])).await;
    completed(submission).await;
}

async fn notify(socket: &mut Socket, id: &str, action: &str, payload: Value) {
    assert_eq!(station_call(socket, id, action, payload).await, json!({}));
}

#[tokio::test]
async fn a_triggered_log_idle_resolves_the_log_job_and_is_the_triggers_evidence() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let log = command(
        "log-job",
        "GetLog",
        "urn:uob:ocpp16:GetLogReference:1",
        &json!({"logType":"SecurityLog","requestId":41,
            "oldestTimestamp":"2026-01-01T00:00:00Z","retryInterval":5}),
    );
    start(
        &client,
        &fixture,
        &mut socket,
        log,
        json!({"status":"Accepted","filename":"security-41.log"}),
    )
    .await;
    let status = json!({"status":"Uploading","requestId":41});
    notify(&mut socket, "up", "LogStatusNotification", status).await;

    let (_, _) = answered(
        &client,
        &fixture,
        &mut socket,
        extended_command("ask-log", "LogStatusNotification", None),
        json!({"status":"Accepted"}),
    )
    .await;
    // N01.FR.12: Idle without requestId, sent for the trigger.
    notify(
        &mut socket,
        "idle",
        "LogStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;

    let job = result(&client, &fixture, "log-job").await["diagnostics_16"]["job"].clone();
    assert_eq!(job["state"], "station_idle");
    assert_eq!(job["last_status"], "Uploading");
    let observed = observation(
        &client,
        &fixture,
        "ask-log",
        "observed",
        Duration::from_secs(8),
    )
    .await;
    assert_eq!(
        observed["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_triggered_signed_firmware_idle_resolves_the_firmware_job_and_is_the_triggers_evidence() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let update = command(
        "firmware-job",
        "SignedUpdateFirmware",
        "urn:uob:ocpp16:SignedUpdateFirmwareReference:1",
        &json!({"requestId":4242,"artifactReference":"station-fw-2.0-signed.bin",
            "retrieveDateTime":"2026-01-01T00:00:00Z","installDateTime":"2026-01-01T00:05:00Z"}),
    );
    start(
        &client,
        &fixture,
        &mut socket,
        update,
        json!({"status":"Accepted"}),
    )
    .await;
    let status = json!({"status":"Downloading","requestId":4242});
    notify(
        &mut socket,
        "dl",
        "SignedFirmwareStatusNotification",
        status,
    )
    .await;

    answered(
        &client,
        &fixture,
        &mut socket,
        extended_command("ask-firmware", "FirmwareStatusNotification", None),
        json!({"status":"Accepted"}),
    )
    .await;
    // L01.FR.21: a station-wide Idle carries no requestId.
    notify(
        &mut socket,
        "idle",
        "SignedFirmwareStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;

    let job = result(&client, &fixture, "firmware-job").await["firmware_16"]["job"].clone();
    assert_eq!(job["state"], "station_idle");
    assert_eq!(job["last_status"], "Downloading");
    let observed = observation(
        &client,
        &fixture,
        "ask-firmware",
        "observed",
        Duration::from_secs(8),
    )
    .await;
    assert_eq!(
        observed["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
