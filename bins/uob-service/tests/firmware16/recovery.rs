use super::support::*;
use serde_json::json;

#[tokio::test]
async fn daemon_restart_never_replays_and_later_station_facts_resolve_the_job() {
    let (fixture, _) = fixture(false, false);
    let client = client();
    {
        let mut process = fixture.start();
        fixture.ready(&mut process).await;
        let mut socket = fixture.station("station-a").await;
        boot(&mut socket).await;
        fixture.connected(&client, "station-a").await;
        let submission = begin(
            &client,
            &fixture,
            legacy_command("lost-reply", LEGACY_IMAGE),
        );
        let call = receive(&mut socket).await;
        assert_eq!(call[2], "UpdateFirmware");
        // The daemon dies before the station answers.
        drop(process);
        let _ = submission.await;
    }
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let recovered = result(&client, &fixture, "lost-reply").await;
    assert_eq!(recovered["lifecycle"]["stage"], "transmission_uncertain");
    assert_eq!(recovered["firmware_16"]["job"]["state"], "uncertain");
    assert!(recovered["firmware_16"]["reply"].is_null());
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    no_call(&mut socket).await;
    // An unconfirmed job may be replaced; the station's own reports still reconcile.
    notify(
        &mut socket,
        "installing",
        "FirmwareStatusNotification",
        json!({"status":"Installing"}),
    )
    .await;
    assert_eq!(
        job_state(&client, &fixture, "lost-reply").await,
        "installing"
    );
    notify(
        &mut socket,
        "installed",
        "FirmwareStatusNotification",
        json!({"status":"Installed"}),
    )
    .await;
    assert_eq!(
        job_state(&client, &fixture, "lost-reply").await,
        "installed"
    );
    drop(process);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let retained = result(&client, &fixture, "lost-reply").await;
    assert_eq!(retained["firmware_16"]["job"]["state"], "installed");
}

#[tokio::test]
async fn triggered_idle_resolves_a_job_whose_outcome_was_never_reported() {
    let (fixture, _) = fixture(false, true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let submission = begin(&client, &fixture, legacy_command("silent", LEGACY_IMAGE));
    let call = receive(&mut socket).await;
    send(&mut socket, json!([3, call[1], {}])).await;
    completed(submission).await;
    notify(
        &mut socket,
        "dl",
        "FirmwareStatusNotification",
        json!({"status":"Downloading"}),
    )
    .await;
    let trigger = json!({"request_id":"ask-firmware","resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":"TriggerMessage",
        "payload_schema":"urn:OCPP:1.6:2019:12:TriggerMessageRequest",
        "payload":{"requestedMessage":"FirmwareStatusNotification"}}},"expires_at":"2099-01-01T00:00:00Z"});
    let submission = begin(&client, &fixture, trigger);
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "TriggerMessage");
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    completed(submission).await;
    // 1.6 §4.5: Idle is sent only when the station is not busy with firmware.
    notify(
        &mut socket,
        "idle",
        "FirmwareStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;
    let idle = result(&client, &fixture, "silent").await;
    assert_eq!(idle["firmware_16"]["job"]["state"], "station_idle");
    assert_eq!(idle["firmware_16"]["job"]["last_status"], "Downloading");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let trigger = result(&client, &fixture, "ask-firmware").await;
            if trigger["trigger_observation"]["observed"]
                .as_array()
                .is_some_and(|o| !o.is_empty())
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the same notification is also trigger evidence");
}
