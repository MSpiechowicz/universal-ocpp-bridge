use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn a_registered_stations_event_is_journaled_before_the_ack_and_replays_identically() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let mut live = Journal::open(&client, &fixture).await;

    // The charger wrote its local time with an offset; the journal keeps UTC.
    let frame = event_call(
        "sec-1",
        "SettingSystemTime",
        "2026-10-07T08:15:00+02:00",
        Some("clock set by maintenance"),
    );
    notify(&mut socket, frame).await;

    // The acknowledgement is only sent after the commit, so a reader that starts afterwards
    // already finds the record.
    let mut late = Journal::open(&client, &fixture).await;
    let replayed = late.next().await;
    assert_eq!(live.next().await, replayed);
    assert_eq!(replayed["event_type"], "station.security_event.16");
    assert_eq!(replayed["origin"]["kind"], "station");
    assert_eq!(replayed["resource"]["station_id"], "station-a");
    assert!(replayed["resource"]["resource"].is_null());
    assert_eq!(replayed["source_time"], "2026-10-07T06:15:00Z");
    assert_eq!(
        record(&replayed),
        &json!({"event_type":"SettingSystemTime","standard":true,
            "timestamp":"2026-10-07T06:15:00Z","tech_info":"clock set by maintenance"})
    );
    assert!(late.quiet().await);
}

#[tokio::test]
async fn vendor_event_types_are_kept_verbatim_and_every_standard_name_is_marked() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let mut journal = Journal::open(&client, &fixture).await;

    for name in [
        "security-event-vendor-type.json",
        "security-event-firmware-updated.json",
        "security-event-minimal.json",
        "security-event-type-max.json",
        "security-event-tech-info-max.json",
    ] {
        let frame = wire(name);
        let expected = frame[3].clone();
        notify(&mut socket, frame).await;

        let event = journal.next().await;
        let stored = record(&event);
        assert_eq!(stored["event_type"], expected["type"], "{name}");
        assert_eq!(stored["tech_info"], expected["techInfo"], "{name}");
        let standard = matches!(
            expected["type"].as_str().unwrap(),
            "FirmwareUpdated" | "StartupOfTheDevice" | "MemoryExhaustion"
        );
        assert_eq!(stored["standard"], standard, "{name}");
    }
    assert!(journal.quiet().await);
}

#[tokio::test]
async fn credentials_in_tech_info_are_redacted_everywhere_the_daemon_keeps_or_publishes_data() {
    let (fixture, process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let mut journal = Journal::open(&client, &fixture).await;

    notify(
        &mut socket,
        wire("security-event-tech-info-credential.json"),
    )
    .await;
    let event = journal.next().await;
    let stored = record(&event);
    let text = stored["tech_info"].as_str().unwrap();
    assert!(text.contains("[redacted]"), "{text}");
    assert!(text.contains("svc-demo origin=touchscreen"), "{text}");
    assert!(stored["tech_info_redactions"].as_u64().unwrap() >= 2);
    assert_eq!(stored["event_type"], "MaintenanceLoginFailed");

    let mut late = Journal::open(&client, &fixture).await;
    assert_eq!(late.next().await, event);
    for secret in SECRETS {
        assert!(!journal.raw.contains(secret), "live stream");
        assert!(!late.raw.contains(secret), "replayed stream");
        assert!(!contains(&state_bytes(&fixture), secret), "running state");
    }

    // After a clean stop the WAL is folded into the database file; the secret is still absent.
    drop(socket);
    drop(process);
    for secret in SECRETS {
        assert!(!contains(&state_bytes(&fixture), secret), "stopped state");
    }
}

#[tokio::test]
async fn redelivery_on_the_same_connection_and_after_reconnect_is_acknowledged_once_stored() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let first = event_call(
        "once",
        "ResetOrReboot",
        "2026-10-07T06:15:00Z",
        Some("commanded"),
    );
    notify(&mut socket, first.clone()).await;

    // The same timestamp written with an offset is the same instant, so the same event.
    let mut again = first.clone();
    again[1] = json!("again");
    notify(&mut socket, again).await;
    let mut offset = first.clone();
    offset[1] = json!("offset");
    offset[3]["timestamp"] = json!("2026-10-07T08:15:00+02:00");
    notify(&mut socket, offset).await;
    assert_eq!(stored(&client, &fixture).await, 1);

    drop(socket);
    let mut socket = connected(&fixture, &client).await;
    let mut after = first.clone();
    after[1] = json!("after-reconnect");
    notify(&mut socket, after).await;
    assert_eq!(stored(&client, &fixture).await, 1);

    // A different type, instant or text is a new event.
    for (id, kind, timestamp, text) in [
        (
            "other-type",
            "InvalidMessages",
            "2026-10-07T06:15:00Z",
            Some("commanded"),
        ),
        (
            "other-time",
            "ResetOrReboot",
            "2026-10-07T06:15:01Z",
            Some("commanded"),
        ),
        (
            "other-text",
            "ResetOrReboot",
            "2026-10-07T06:15:00Z",
            Some("watchdog"),
        ),
        ("no-text", "ResetOrReboot", "2026-10-07T06:15:00Z", None),
    ] {
        notify(&mut socket, event_call(id, kind, timestamp, text)).await;
    }
    assert_eq!(stored(&client, &fixture).await, 5);
}

#[tokio::test]
async fn events_are_always_answered_and_never_become_commands_or_capabilities() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    notify(&mut socket, wire("security-event-reset-or-reboot.json")).await;

    let snapshot: Value = fixture.snapshot(&client, "station-a").await;
    assert!(
        !snapshot["capabilities"]["operations"]
            .to_string()
            .contains("ExtendedTriggerMessage")
    );
    let heartbeat = station_call(&mut socket, "beat", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
    no_call(&mut socket).await;
}
