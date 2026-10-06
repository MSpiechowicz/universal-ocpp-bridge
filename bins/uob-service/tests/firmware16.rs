#![cfg(unix)]
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "firmware16/recovery.rs"]
mod recovery;
#[path = "firmware16/signed.rs"]
mod signed;
#[allow(dead_code)]
#[path = "firmware16/support.rs"]
mod support;
use serde_json::{Value, json};
use support::*;

#[tokio::test]
async fn actual_daemon_runs_a_legacy_update_through_reboot_to_installed() {
    let (fixture, port) = fixture(false, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let snapshot = fixture.snapshot(&client, "station-a").await;
    let advertised = snapshot["capabilities"]["operations"].to_string();
    assert!(advertised.contains("\"UpdateFirmware\""));
    assert!(!advertised.contains("SignedUpdateFirmware"));

    let submission = begin(&client, &fixture, legacy_command("legacy-1", LEGACY_IMAGE));
    let call = receive(&mut socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[1], "legacy-1");
    assert_eq!(call[2], "UpdateFirmware");
    let location = format!("http://127.0.0.1:{port}/artifacts/{LEGACY_IMAGE}");
    assert_eq!(
        call[3],
        json!({"location":location,"retrieveDate":"2026-01-01T00:00:00Z","retries":2,"retryInterval":30})
    );
    send(&mut socket, json!([3, "legacy-1", {}])).await;
    let accepted = completed(submission).await;
    assert_eq!(accepted["schema_version"], json!({"major":1,"revision":14}));
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    let evidence = &accepted["firmware_16"];
    assert_eq!(evidence["action"], "UpdateFirmware");
    assert_eq!(evidence["reply"], json!({"kind":"acknowledged"}));
    assert_eq!(evidence["job"]["state"], "accepted");
    assert_eq!(evidence["artifact"]["signed"], false);
    assert!(!accepted.to_string().contains("http://"));
    download(&location, &evidence["artifact"], &image(1)).await;

    for (index, status) in ["Downloading", "Downloaded", "Installing"]
        .into_iter()
        .enumerate()
    {
        let id = format!("status-{index}");
        notify(
            &mut socket,
            &id,
            "FirmwareStatusNotification",
            json!({"status":status}),
        )
        .await;
        let state = job_state(&client, &fixture, "legacy-1").await;
        assert_eq!(state, status.to_lowercase());
    }
    // The station reboots into the new image and reports success on a new socket.
    drop(socket);
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    no_call(&mut socket).await;
    notify(
        &mut socket,
        "installed",
        "FirmwareStatusNotification",
        json!({"status":"Installed"}),
    )
    .await;
    let installed = result(&client, &fixture, "legacy-1").await;
    assert_eq!(installed["firmware_16"]["job"]["state"], "installed");
    assert_eq!(installed["firmware_16"]["job"]["notifications"], 4);
    assert_eq!(installed["firmware_16"]["reply"], evidence["reply"]);
    assert_eq!(installed["firmware_16"]["artifact"], evidence["artifact"]);
    // A late duplicate never reopens the settled job.
    notify(
        &mut socket,
        "late",
        "FirmwareStatusNotification",
        json!({"status":"Downloading"}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "legacy-1").await, "installed");
}

#[tokio::test]
async fn refusals_never_reach_the_station_and_callerror_is_native_evidence() {
    let (fixture, _) = fixture(false, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;

    let unprivileged = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&legacy_command("control-grant", LEGACY_IMAGE))
        .send()
        .await
        .unwrap();
    assert!(unprivileged.status().is_client_error());
    let mut located = legacy_command("caller-location", LEGACY_IMAGE);
    located["operation"]["parameters"]["payload"]["location"] = json!("http://attacker.invalid/x");
    let refused = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&located)
        .send()
        .await
        .unwrap();
    assert!(refused.status().is_client_error());
    let mut connector = legacy_command("connector-scope", LEGACY_IMAGE);
    connector["resource"]["resource"] = json!({"kind":"connector","connector_id":"one"});
    connector["resource"]["native_protocol_reference"] =
        json!({"protocol":"ocpp16","connector_id":1});
    let scoped = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&connector)
        .send()
        .await
        .unwrap();
    assert!(scoped.status().is_client_error() || scoped.status() == 202);
    if scoped.status() == 202 {
        let value = scoped.json::<Value>().await.unwrap()["result"].clone();
        assert_eq!(value["lifecycle"]["stage"], "rejected");
    }
    for (id, body, status, code) in [
        (
            "unknown-artifact",
            legacy_command("unknown-artifact", "missing.bin"),
            400,
            "policy_rejected",
        ),
        (
            "wrong-kind",
            legacy_command("wrong-kind", SIGNED_IMAGE),
            400,
            "invalid_parameters",
        ),
        (
            "wrong-family",
            signed_command("wrong-family", 1, SIGNED_IMAGE),
            422,
            "unsupported_operation",
        ),
    ] {
        let (http, value) = submit_refused(&client, &fixture, body).await;
        assert_eq!(http, status, "{id}");
        no_call(&mut socket).await;
        // An unadvertised family is refused before durable admission, without a result.
        if id != "wrong-family" {
            assert_eq!(value["lifecycle"]["stage"], "rejected", "{id}");
            assert_eq!(value["lifecycle"]["error"]["code"], code, "{id}");
            assert_eq!(job_state(&client, &fixture, id).await, "not_sent");
        }
    }
}

#[tokio::test]
async fn native_callerror_active_jobs_and_unenabled_signed_reports_stay_explicit() {
    let (fixture, _) = fixture(false, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    // L01.FR.20: a signed-only station answers the original message with NotSupported.
    let submission = begin(
        &client,
        &fixture,
        legacy_command("not-supported", LEGACY_IMAGE),
    );
    let call = receive(&mut socket).await;
    send(
        &mut socket,
        json!([4, call[1], "NotSupported", "use SignedUpdateFirmware", {}]),
    )
    .await;
    let value = completed(submission).await;
    assert_eq!(value["lifecycle"]["accepted"], false);
    assert_eq!(
        value["firmware_16"]["reply"],
        json!({"kind":"call_error","code":"NotSupported"})
    );
    assert_eq!(value["firmware_16"]["job"]["state"], "rejected");
    // An active job blocks another legacy request before any byte is sent.
    let submission = begin(&client, &fixture, legacy_command("active", LEGACY_IMAGE));
    let call = receive(&mut socket).await;
    send(&mut socket, json!([3, call[1], {}])).await;
    completed(submission).await;
    let (http, blocked) =
        submit_refused(&client, &fixture, legacy_command("blocked", LEGACY_IMAGE)).await;
    assert_eq!(http, 400);
    assert_eq!(blocked["lifecycle"]["stage"], "rejected");
    assert_eq!(blocked["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
    // The Security Whitepaper notification exists only where that family is enabled.
    send(
        &mut socket,
        json!([2, "signed-status", "SignedFirmwareStatusNotification", {"status":"Downloading","requestId":1}]),
    )
    .await;
    let reply = receive(&mut socket).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "NotImplemented");
}
