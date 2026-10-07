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
        let submission = begin(&client, &fixture, command("lost-reply", "SecurityLog", 5));
        let call = receive(&mut socket).await;
        assert_eq!(call[2], "GetLog");
        // The daemon dies before the station answers.
        drop(process);
        let _ = submission.await;
    }
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let recovered = result(&client, &fixture, "lost-reply").await;
    assert_eq!(recovered["lifecycle"]["stage"], "transmission_uncertain");
    assert_eq!(recovered["diagnostics_201"]["job"]["state"], "uncertain");
    assert!(recovered["diagnostics_201"]["reply"].is_null());
    let mut socket = connected(&fixture, &client).await;
    no_call(&mut socket).await;
    // The in-memory demo destination did not survive the restart, so the upload fails.
    notify(
        &mut socket,
        "uploading",
        json!({"status":"Uploading","requestId":5}),
    )
    .await;
    notify(
        &mut socket,
        "failed",
        json!({"status":"UploadFailure","requestId":5}),
    )
    .await;
    assert_eq!(
        job(&client, &fixture, "lost-reply").await["state"],
        "upload_failed"
    );
    drop(process);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let retained = result(&client, &fixture, "lost-reply").await;
    assert_eq!(retained["diagnostics_201"]["job"]["state"], "upload_failed");
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
        command("silent", "SecurityLog", 6),
        json!({"status":"Accepted"}),
    )
    .await;
    notify(
        &mut socket,
        "up",
        json!({"status":"Uploading","requestId":6}),
    )
    .await;
    let trigger = json!({"request_id":"ask-log","resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp201","action":"TriggerMessage",
        "payload_schema":"urn:OCPP:Cp:2:2020:3:TriggerMessageRequest",
        "payload":{"requestedMessage":"LogStatusNotification"}}},"expires_at":"2099-01-01T00:00:00Z"});
    let submission = begin(&client, &fixture, trigger);
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "TriggerMessage");
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    completed(submission).await;
    // N01.FR.13: Idle after the station finished, with no upload identity.
    notify(&mut socket, "idle", json!({"status":"Idle"})).await;
    let idle = result(&client, &fixture, "silent").await;
    assert_eq!(idle["diagnostics_201"]["job"]["state"], "station_idle");
    assert_eq!(idle["diagnostics_201"]["job"]["last_status"], "Uploading");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let trigger = result(&client, &fixture, "ask-log").await;
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
