#![cfg(unix)]
#[path = "remote_trigger_201/support.rs"]
mod support;

use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;
use support::{CONTROL, Fixture, READ, command, result, station_call, stop, submit, until};
use uob_application::OperationalStore;
use uob_contracts::{RequestId, StationEvent, TransactionSnapshot};
use uob_storage_adapter::SqliteOperationalStore;

#[tokio::test]
async fn native_response_and_scoped_messages_are_separate_durable_evidence() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut socket = fixture.station().await;
    fixture.connected(&client).await;

    boot_and_heartbeat(&client, &fixture, &mut socket).await;

    status_notifications(&client, &fixture, &mut socket).await;

    meter_and_log(&client, &fixture, &mut socket).await;

    certificate_and_denial(&client, &fixture, &mut socket).await;

    drop(socket);
    stop(child);
    let database: SqliteOperationalStore<Value, StationEvent, TransactionSnapshot, String> =
        SqliteOperationalStore::open(fixture.root.join("state/charging.sqlite3"), 32).unwrap();
    let saved = database
        .command_result_by_request_id(RequestId::new("status201".to_owned()).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap()["trigger_observation_201"]["status"],
        "observed"
    );
}

#[tokio::test]
async fn triggered_transaction_requires_trigger_reason_and_reconnect_does_not_replay() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut socket = fixture.station().await;
    fixture.connected(&client).await;
    assert_eq!(boot_notification(&mut socket, "boot").await[0], 3);

    let pending = submit(
        &client,
        &fixture,
        &mut socket,
        "tx201",
        "TransactionEvent",
        Some((1, Some(1))),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(pending["trigger_observation_201"]["status"], "pending");
    let instant = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert_eq!(
        station_call(
            &mut socket,
            "start",
            "TransactionEvent",
            json!({"eventType":"Started","timestamp":instant,"triggerReason":"CablePluggedIn",
            "seqNo":0,"transactionInfo":{"transactionId":"tx-a"},
            "evse":{"id":1,"connectorId":1}})
        )
        .await[0],
        3
    );
    assert_eq!(
        result(&client, &fixture, "tx201").await["trigger_observation_201"]["status"],
        "pending"
    );

    drop(socket);
    stop(child);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let mut socket = fixture.station().await;
    fixture.connected(&client).await;
    let fresh = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
    assert!(
        fresh.is_err(),
        "a reconnect must not replay the trigger command"
    );
    assert_eq!(
        boot_notification(&mut socket, "boot-after-restart").await[2]["status"],
        "Accepted"
    );
    let instant = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert_eq!(
        station_call(
            &mut socket,
            "trigger-update",
            "TransactionEvent",
            json!({"eventType":"Updated","timestamp":instant,"triggerReason":"Trigger",
            "seqNo":1,"transactionInfo":{"transactionId":"tx-a"},
            "evse":{"id":1,"connectorId":1}})
        )
        .await[0],
        3
    );
    let observed = until(&client, &fixture, "tx201", "observed").await;
    assert_eq!(
        observed["trigger_observation_201"]["observed"][0]["target"],
        json!({"kind":"connector","id":1,"connector_id":1})
    );
    drop(socket);
    stop(child);
    let database: SqliteOperationalStore<Value, StationEvent, TransactionSnapshot, String> =
        SqliteOperationalStore::open(fixture.root.join("state/charging.sqlite3"), 32).unwrap();
    let saved = database
        .command_result_by_request_id(RequestId::new("tx201".to_owned()).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap()["trigger_observation_201"]["status"],
        "observed"
    );
}

async fn boot_and_heartbeat(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut support::Socket,
) {
    assert_eq!(
        client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(CONTROL)
            .json(&command("denied-201", "BootNotification", None))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let boot = submit(
        client,
        fixture,
        socket,
        "boot201",
        "BootNotification",
        None,
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(boot["trigger_observation_201"]["status"], "pending");
    assert_eq!(
        boot_notification(socket, "boot").await[2]["status"],
        "Accepted"
    );
    until(client, fixture, "boot201", "observed").await;
    let snapshot: Value = client
        .get(fixture.url("/api/v1/stations/station-a"))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let advertises_trigger = |capabilities: &Value| {
        capabilities["operations"]
            .as_array()
            .is_some_and(|operations| {
                operations.iter().any(|entry| {
                    entry["operation"]
                        == json!({
                            "kind":"protocol_action","protocol":"ocpp201","action":"TriggerMessage"
                        })
                })
            })
    };
    assert!(advertises_trigger(&snapshot["capabilities"]));
    assert_eq!(
        snapshot["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| advertises_trigger(&entry["capabilities"]))
            .count(),
        5
    );
    let heartbeat = submit(
        client,
        fixture,
        socket,
        "heartbeat201",
        "Heartbeat",
        None,
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(heartbeat["trigger_observation_201"]["status"], "pending");
    assert_eq!(
        station_call(socket, "heartbeat", "Heartbeat", json!({})).await[0],
        3
    );
    until(client, fixture, "heartbeat201", "observed").await;
}

async fn status_notifications(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut support::Socket,
) {
    let status = submit(
        client,
        fixture,
        socket,
        "status201",
        "StatusNotification",
        Some((1, Some(1))),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(status["trigger_observation_201"]["status"], "pending");
    assert_eq!(
        station_call(
            socket,
            "wrong-connector",
            "StatusNotification",
            json!({"evseId":1,"connectorId":2,"connectorStatus":"Available",
            "timestamp":"2026-09-27T00:00:00Z"})
        )
        .await[0],
        3
    );
    assert_eq!(
        result(client, fixture, "status201").await["trigger_observation_201"]["status"],
        "pending"
    );
    assert_eq!(
        station_call(
            socket,
            "invalid-connector",
            "StatusNotification",
            json!({"evseId":1,"connectorId":99,"connectorStatus":"Available",
            "timestamp":"2026-09-27T00:00:01Z"})
        )
        .await[0],
        4
    );
    assert_eq!(
        result(client, fixture, "status201").await["trigger_observation_201"]["status"],
        "pending"
    );
    assert_eq!(
        station_call(
            socket,
            "exact-connector",
            "StatusNotification",
            json!({"evseId":1,"connectorId":1,"connectorStatus":"Available",
            "timestamp":"2026-09-27T00:00:02Z"})
        )
        .await[0],
        3
    );
    let observed = until(client, fixture, "status201", "observed").await;
    assert_eq!(
        observed["trigger_observation_201"]["native_response"]["status"],
        "Accepted"
    );
    assert_eq!(
        observed["trigger_observation_201"]["observed"][0]["target"],
        json!({"kind":"connector","id":1,"connector_id":1})
    );
}

async fn meter_and_log(client: &reqwest::Client, fixture: &Fixture, socket: &mut support::Socket) {
    let meter = submit(
        client,
        fixture,
        socket,
        "meter201",
        "MeterValues",
        Some((1, None)),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(meter["trigger_observation_201"]["status"], "pending");
    assert_eq!(
        station_call(
            socket,
            "wrong-meter",
            "MeterValues",
            json!({"evseId":2,"meterValue":[{"timestamp":"2026-09-27T00:00:03Z",
            "sampledValue":[{"value":7.5}]}]})
        )
        .await[0],
        3
    );
    assert_eq!(
        result(client, fixture, "meter201").await["trigger_observation_201"]["status"],
        "pending"
    );
    assert_eq!(
        station_call(
            socket,
            "exact-meter",
            "MeterValues",
            json!({"evseId":1,"meterValue":[{"timestamp":"2026-09-27T00:00:04Z",
            "sampledValue":[{"value":8.5}]}]})
        )
        .await[0],
        3
    );
    until(client, fixture, "meter201", "observed").await;
    let log = submit(
        client,
        fixture,
        socket,
        "log201",
        "LogStatusNotification",
        None,
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(log["trigger_observation_201"]["status"], "pending");
    assert_eq!(
        station_call(
            socket,
            "invalid-log",
            "LogStatusNotification",
            json!({"status":"fabricated"})
        )
        .await[0],
        4
    );
    assert_eq!(
        result(client, fixture, "log201").await["trigger_observation_201"]["status"],
        "pending"
    );
    assert_eq!(
        station_call(
            socket,
            "log",
            "LogStatusNotification",
            json!({"status":"Idle"})
        )
        .await[0],
        4
    );
    until(client, fixture, "log201", "observed").await;
}

async fn certificate_and_denial(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut support::Socket,
) {
    let certificate = submit(
        client,
        fixture,
        socket,
        "certificate201",
        "SignV2GCertificate",
        Some((1, None)),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(certificate["trigger_observation_201"]["status"], "pending");
    assert_eq!(
        station_call(
            socket,
            "certificate",
            "SignCertificate",
            json!({"csr":"-----BEGIN CERTIFICATE REQUEST-----",
            "certificateType":"V2GCertificate"})
        )
        .await[0],
        4
    );
    let receipt = until(client, fixture, "certificate201", "unattributable").await;
    assert_eq!(
        receipt["trigger_observation_201"]["observed"][0]["target"],
        json!({"kind":"station"})
    );

    let rejected = submit(
        client,
        fixture,
        socket,
        "rejected201",
        "Heartbeat",
        None,
        json!({"status":"Rejected","statusInfo":{"reasonCode":"NotAvailable"}}),
    )
    .await;
    assert_eq!(rejected["trigger_observation_201"]["status"], "unsupported");
    assert_eq!(
        station_call(socket, "later-heartbeat", "Heartbeat", json!({})).await[0],
        3
    );
    assert_eq!(
        result(client, fixture, "rejected201").await["trigger_observation_201"]["status"],
        "unsupported"
    );
}

async fn boot_notification(socket: &mut support::Socket, id: &str) -> Value {
    station_call(
        socket,
        id,
        "BootNotification",
        json!({"chargingStation":{"vendorName":"A","model":"A"},"reason":"PowerUp"}),
    )
    .await
}
