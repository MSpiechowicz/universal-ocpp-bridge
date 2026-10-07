use super::support::*;
use serde_json::json;

#[tokio::test]
async fn daemon_restart_never_replays_and_later_station_facts_resolve_the_job() {
    let (fixture, _) = fixture(true, false);
    let client = client();
    {
        let mut process = fixture.start();
        fixture.ready(&mut process).await;
        let mut socket = connected(&fixture, &client).await;
        let submission = begin(&client, &fixture, command("lost-reply", 77, SIGNED_IMAGE));
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
    assert_eq!(recovered["firmware_201"]["job"]["state"], "uncertain");
    assert!(recovered["firmware_201"]["reply"].is_null());
    let mut socket = connected(&fixture, &client).await;
    no_call(&mut socket).await;
    // The station's report with the exact requestId still reconciles the job.
    notify(
        &mut socket,
        "installing",
        json!({"status":"Installing","requestId":77}),
    )
    .await;
    assert_eq!(
        job_state(&client, &fixture, "lost-reply").await,
        "installing"
    );
    notify(
        &mut socket,
        "installed",
        json!({"status":"Installed","requestId":77}),
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
    assert_eq!(retained["firmware_201"]["job"]["state"], "installed");
}

#[tokio::test]
async fn triggered_idle_resolves_a_stalled_job_and_counts_as_trigger_evidence() {
    let (fixture, _) = fixture(true, true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    answered(
        &client,
        &fixture,
        &mut socket,
        command("silent", 5, SIGNED_IMAGE),
        json!({"status":"Accepted"}),
    )
    .await;
    notify(
        &mut socket,
        "dl",
        json!({"status":"Downloading","requestId":5}),
    )
    .await;
    let trigger = json!({"request_id":"ask-firmware","resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp201","action":"TriggerMessage",
        "payload_schema":"urn:OCPP:Cp:2:2020:3:TriggerMessageRequest",
        "payload":{"requestedMessage":"FirmwareStatusNotification"}}},"expires_at":"2099-01-01T00:00:00Z"});
    let submission = begin(&client, &fixture, trigger);
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "TriggerMessage");
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    completed(submission).await;
    // L01.FR.25: Idle after the station finished, with no update identity (L01.FR.20).
    notify(&mut socket, "idle", json!({"status":"Idle"})).await;
    let idle = result(&client, &fixture, "silent").await;
    assert_eq!(idle["firmware_201"]["job"]["state"], "station_idle");
    assert_eq!(idle["firmware_201"]["job"]["last_status"], "Downloading");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let trigger = result(&client, &fixture, "ask-firmware").await;
            if trigger["trigger_observation_201"]["observed"]
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
