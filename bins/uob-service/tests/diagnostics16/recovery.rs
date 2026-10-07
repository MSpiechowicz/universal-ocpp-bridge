use super::support::*;
use serde_json::json;

#[tokio::test]
async fn daemon_restart_never_replays_and_later_station_facts_resolve_the_job() {
    let (fixture, _) = fixture(true, false, false);
    let client = client();
    {
        let mut process = fixture.start();
        fixture.ready(&mut process).await;
        let mut socket = fixture.station("station-a").await;
        boot(&mut socket).await;
        fixture.connected(&client, "station-a").await;
        let submission = begin(&client, &fixture, diagnostics_command("lost-reply"));
        let call = receive(&mut socket).await;
        assert_eq!(call[2], "GetDiagnostics");
        // The daemon dies before the station answers.
        drop(process);
        let _ = submission.await;
    }
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let recovered = result(&client, &fixture, "lost-reply").await;
    assert_eq!(recovered["lifecycle"]["stage"], "transmission_uncertain");
    assert_eq!(recovered["diagnostics_16"]["job"]["state"], "uncertain");
    assert!(recovered["diagnostics_16"]["reply"].is_null());
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    no_call(&mut socket).await;
    // The in-memory demo destination did not survive the restart, so the upload fails.
    notify(
        &mut socket,
        "uploading",
        "DiagnosticsStatusNotification",
        json!({"status":"Uploading"}),
    )
    .await;
    notify(
        &mut socket,
        "failed",
        "DiagnosticsStatusNotification",
        json!({"status":"UploadFailed"}),
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
    assert_eq!(retained["diagnostics_16"]["job"]["state"], "upload_failed");
}

#[tokio::test]
async fn triggered_idle_resolves_a_job_and_is_also_trigger_evidence() {
    let (fixture, _) = fixture(true, false, true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let submission = begin(&client, &fixture, diagnostics_command("silent"));
    let call = receive(&mut socket).await;
    send(&mut socket, json!([3, call[1], {"fileName":"s.log"}])).await;
    completed(submission).await;
    notify(
        &mut socket,
        "up",
        "DiagnosticsStatusNotification",
        json!({"status":"Uploading"}),
    )
    .await;
    let trigger = json!({"request_id":"ask-diagnostics","resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":"TriggerMessage",
        "payload_schema":"urn:OCPP:1.6:2019:12:TriggerMessageRequest",
        "payload":{"requestedMessage":"DiagnosticsStatusNotification"}}},"expires_at":"2099-01-01T00:00:00Z"});
    let submission = begin(&client, &fixture, trigger);
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "TriggerMessage");
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    completed(submission).await;
    // 1.6 §4.4: Idle is sent only after a trigger when no upload is busy.
    notify(
        &mut socket,
        "idle",
        "DiagnosticsStatusNotification",
        json!({"status":"Idle"}),
    )
    .await;
    let idle = job(&client, &fixture, "silent").await;
    assert_eq!(idle["state"], "station_idle");
    assert_eq!(idle["last_status"], "Uploading");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let trigger = result(&client, &fixture, "ask-diagnostics").await;
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
