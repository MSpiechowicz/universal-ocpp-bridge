use super::support::*;
use serde_json::json;
use std::time::Duration;

const FULL: Options = Options {
    extended: true,
    plain_trigger: true,
    log: true,
    signed: true,
};
const SHORT: Duration = Duration::from_secs(8);

#[tokio::test]
async fn the_log_status_trigger_sends_one_call_and_is_observed_by_the_triggered_idle() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    // The action is offered on the station root and on both connectors.
    let snapshot = fixture.snapshot(&client, "station-a").await.to_string();
    assert_eq!(snapshot.matches("ExtendedTriggerMessage").count(), 3);

    let (call, pending) = answered(
        &client,
        &fixture,
        &mut socket,
        extended_command("ask-log", "LogStatusNotification", None),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(call[3], json!({"requestedMessage":"LogStatusNotification"}));
    no_call(&mut socket).await;
    let observation = &pending["trigger_observation"];
    assert_eq!(observation["requested_class"], "LogStatusNotification");
    assert_eq!(observation["status"], "pending");
    assert_eq!(observation["native_response"], "Accepted");
    assert_eq!(pending["schema_version"], json!({"major":1,"revision":19}));

    // N01.FR.12: a triggered Idle carries no requestId.
    notify_status(
        &mut socket,
        "idle",
        "LogStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;
    let observed = observation_of(&client, &fixture, "ask-log").await;
    assert_eq!(
        observed["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(observed["schema_version"], json!({"major":1,"revision":19}));
}

async fn notify_status(socket: &mut Socket, id: &str, action: &str, payload: serde_json::Value) {
    assert_eq!(station_call(socket, id, action, payload).await, json!({}));
}

async fn observation_of(
    client: &reqwest::Client,
    fixture: &Fixture,
    id: &str,
) -> serde_json::Value {
    observation(client, fixture, id, "observed", SHORT).await
}

#[tokio::test]
async fn the_firmware_trigger_needs_the_signed_notification_and_a_plain_one_never_counts() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let (call, pending) = answered(
        &client,
        &fixture,
        &mut socket,
        extended_command("ask-firmware", "FirmwareStatusNotification", None),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(
        call[3],
        json!({"requestedMessage":"FirmwareStatusNotification"})
    );
    let observation = &pending["trigger_observation"];
    assert_eq!(
        observation["requested_class"],
        "SignedFirmwareStatusNotification"
    );
    assert_eq!(pending["schema_version"], json!({"major":1,"revision":19}));

    // The legacy message is acknowledged but is not the evidence this request asked for.
    notify_status(
        &mut socket,
        "plain",
        "FirmwareStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still = result(&client, &fixture, "ask-firmware").await;
    assert_eq!(still["trigger_observation"]["status"], "pending");

    // L01.FR.21: a station-wide Idle has no requestId.
    notify_status(
        &mut socket,
        "signed",
        "SignedFirmwareStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;
    observation_of(&client, &fixture, "ask-firmware").await;
    no_call(&mut socket).await;
}

#[tokio::test]
async fn the_station_can_reject_and_an_unreplied_request_ends_absent() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    for (id, requested, reply) in [
        ("rejected", "LogStatusNotification", "Rejected"),
        (
            "unimplemented",
            "FirmwareStatusNotification",
            "NotImplemented",
        ),
    ] {
        let (_, value) = answered(
            &client,
            &fixture,
            &mut socket,
            extended_command(id, requested, None),
            json!({"status":reply}),
        )
        .await;
        assert_eq!(
            value["trigger_observation"]["status"], "unsupported",
            "{id}"
        );
        assert_eq!(value["trigger_observation"]["native_response"], reply);
    }

    // Accepted, but the station never sends the message: the window closes as absent.
    let (_, value) = answered(
        &client,
        &fixture,
        &mut socket,
        extended_command("silent", "LogStatusNotification", None),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(value["trigger_observation"]["status"], "pending");
    let absent = observation(
        &client,
        &fixture,
        "silent",
        "absent",
        Duration::from_secs(75),
    )
    .await;
    assert!(
        absent["trigger_observation"]["observed"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

#[tokio::test]
async fn a_delayed_reply_does_not_block_heartbeats_and_the_station_scope_classes_work() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let submission = begin(
        &client,
        &fixture,
        extended_command("slow", "MeterValues", Some(1)),
    );
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "ExtendedTriggerMessage");
    assert_eq!(
        call[3],
        json!({"requestedMessage":"MeterValues","connectorId":1})
    );
    let heartbeat = station_call(&mut socket, "beat", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    let value = completed(submission).await;
    assert_eq!(
        value["trigger_observation"]["requested_class"],
        "MeterValues"
    );
    assert_eq!(value["schema_version"], json!({"major":1,"revision":2}));
}

/// The refusal code of a command that never reached the station.
fn code(status: u16, body: &serde_json::Value) -> String {
    let found = body["lifecycle"]["error"]["code"]
        .as_str()
        .or_else(|| body["error"].as_str());
    format!("{status} {}", found.unwrap_or("-"))
}

#[tokio::test]
async fn invalid_and_unauthorized_requests_are_refused_before_anything_is_sent() {
    let (fixture, _process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let mut wrong_schema = extended_command("schema", "Heartbeat", None);
    wrong_schema["operation"]["parameters"]["payload_schema"] =
        json!("urn:OCPP:1.6:2019:12:TriggerMessageRequest");
    let mut extra = extended_command("extra", "Heartbeat", None);
    extra["operation"]["parameters"]["payload"]["evseId"] = json!(1);
    let mut plain = extended_command("plain", "LogStatusNotification", None);
    plain["operation"]["parameters"]["action"] = json!("TriggerMessage");
    plain["operation"]["parameters"]["payload_schema"] =
        json!("urn:OCPP:1.6:2019:12:TriggerMessageRequest");

    for (id, body, token, expected) in [
        (
            "sign",
            extended_command("sign", "SignChargePointCertificate", None),
            PRIVILEGED,
            // The management layer validates the value against the offered descriptor first.
            "400 command.invalid_schema_or_payload",
        ),
        (
            "zero",
            extended_command("zero", "MeterValues", Some(0)),
            PRIVILEGED,
            "400 command.invalid_schema_or_payload",
        ),
        (
            "unknown",
            extended_command("unknown", "DiagnosticsStatusNotification", None),
            PRIVILEGED,
            "400 command.invalid_schema_or_payload",
        ),
        (
            "schema",
            wrong_schema,
            PRIVILEGED,
            "400 command.invalid_schema_or_payload",
        ),
        (
            "extra",
            extra,
            PRIVILEGED,
            "400 command.invalid_schema_or_payload",
        ),
        // The plain action never offers the Whitepaper classes.
        (
            "plain",
            plain,
            PRIVILEGED,
            "400 command.invalid_schema_or_payload",
        ),
        (
            "unprivileged",
            extended_command("unprivileged", "Heartbeat", None),
            CONTROL,
            "403 command.unauthorized",
        ),
    ] {
        let (status, value) = refused(&client, &fixture, token, &body).await;
        assert_eq!(code(status, &value), expected, "{id}");
        no_call(&mut socket).await;
    }
}

#[tokio::test]
async fn a_class_without_its_enabling_capability_or_a_disabled_option_is_unsupported() {
    // Extended triggers without log or signed firmware: those two classes are not offered.
    let (fixture, process) = launch(Options {
        extended: true,
        ..Options::default()
    })
    .await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    for requested in ["LogStatusNotification", "FirmwareStatusNotification"] {
        let body = extended_command(requested, requested, None);
        let (status, value) = refused(&client, &fixture, PRIVILEGED, &body).await;
        assert_eq!(
            code(status, &value),
            "422 unsupported_operation",
            "{requested}"
        );
        no_call(&mut socket).await;
    }
    drop(socket);
    drop(process);

    // Plain triggers only: the extended action is not advertised at all.
    let (fixture, _process) = launch(Options {
        plain_trigger: true,
        log: true,
        ..Options::default()
    })
    .await;
    let mut socket = connected(&fixture, &client).await;
    let body = extended_command("off", "Heartbeat", None);
    let (status, value) = refused(&client, &fixture, PRIVILEGED, &body).await;
    assert_eq!(code(status, &value), "422 command.unsupported_schema");
    no_call(&mut socket).await;
    let advertised = fixture.snapshot(&client, "station-a").await.to_string();
    assert!(!advertised.contains("ExtendedTriggerMessage"));
    assert!(advertised.contains("\"TriggerMessage\""));
}
