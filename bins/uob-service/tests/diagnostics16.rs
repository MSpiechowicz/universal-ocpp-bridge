#![cfg(unix)]
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "diagnostics16/recovery.rs"]
mod recovery;
#[path = "diagnostics16/security_log.rs"]
mod security_log;
#[allow(dead_code)]
#[path = "diagnostics16/support.rs"]
mod support;
use serde_json::{Value, json};
use support::*;

#[tokio::test]
async fn actual_daemon_confirms_a_legacy_diagnostics_upload_against_the_stored_file() {
    let (fixture, port) = fixture(true, false, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let snapshot = fixture.snapshot(&client, "station-a").await;
    let advertised = snapshot["capabilities"]["operations"].to_string();
    assert!(advertised.contains("\"GetDiagnostics\""));
    assert!(!advertised.contains("\"GetLog\""));

    let submission = begin(&client, &fixture, diagnostics_command("diag-1"));
    let call = receive(&mut socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[1], "diag-1");
    assert_eq!(call[2], "GetDiagnostics");
    let location = call[3]["location"].as_str().unwrap().to_owned();
    assert!(
        location.starts_with(&format!("http://127.0.0.1:{port}/uploads/")),
        "{location}"
    );
    assert_eq!(
        call[3],
        json!({"location":location,"startTime":"2026-01-01T00:00:00Z",
            "stopTime":"2026-01-02T00:00:00Z","retries":1})
    );
    send(&mut socket, json!([3, "diag-1", {"fileName":"diag-a.log"}])).await;
    let accepted = completed(submission).await;
    assert_eq!(accepted["schema_version"], json!({"major":1,"revision":16}));
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    let evidence = &accepted["diagnostics_16"];
    assert_eq!(evidence["action"], "GetDiagnostics");
    assert_eq!(
        evidence["reply"],
        json!({"kind":"diagnostics","file_name":"diag-a.log"})
    );
    assert_eq!(
        evidence["destination"],
        json!({"log_type":"DiagnosticsLog","maximum_bytes":65536,"test_only":true})
    );
    assert_eq!(evidence["job"]["state"], "accepted");
    assert!(!accepted.to_string().contains("/uploads/"));

    notify(
        &mut socket,
        "uploading",
        "DiagnosticsStatusNotification",
        json!({"status":"Uploading"}),
    )
    .await;
    assert_eq!(job(&client, &fixture, "diag-1").await["state"], "uploading");
    let bytes = log_bytes(3);
    assert_eq!(upload(&location, "diag-a.log", &bytes).await, 201);
    notify(
        &mut socket,
        "uploaded",
        "DiagnosticsStatusNotification",
        json!({"status":"Uploaded"}),
    )
    .await;
    let settled = result(&client, &fixture, "diag-1").await;
    let job = &settled["diagnostics_16"]["job"];
    assert_eq!(job["state"], "uploaded");
    assert_eq!(job["notifications"], 2);
    assert_eq!(job["last_status"], "Uploaded");
    assert_eq!(
        job["upload"],
        json!({"sha256":sha256(&bytes),"size_bytes":bytes.len()})
    );
    assert_eq!(settled["diagnostics_16"]["reply"], evidence["reply"]);
    assert!(!settled.to_string().contains("/uploads/"));
    // A late duplicate never reopens the settled job.
    notify(
        &mut socket,
        "late",
        "DiagnosticsStatusNotification",
        json!({"status":"Uploading"}),
    )
    .await;
    assert_eq!(
        self::job(&client, &fixture, "diag-1").await["state"],
        "uploaded"
    );
}

#[tokio::test]
async fn refusals_never_reach_the_station_and_native_replies_stay_explicit() {
    let (fixture, _) = fixture(true, false, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;

    let unprivileged = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&diagnostics_command("control-grant"))
        .send()
        .await
        .unwrap();
    assert!(unprivileged.status().is_client_error());
    let mut located = diagnostics_command("caller-location");
    located["operation"]["parameters"]["payload"]["location"] = json!("ftp://attacker.invalid/");
    let (http, _) = submit_refused(&client, &fixture, located).await;
    assert!((400..500).contains(&http));
    // GetLog is not enabled on this station: refused before durable admission.
    let (http, _) =
        submit_refused(&client, &fixture, log_command("log-off", "SecurityLog", 1)).await;
    assert_eq!(http, 422);
    no_call(&mut socket).await;

    // §5.9: a reply without a file name means nothing will be uploaded.
    let submission = begin(&client, &fixture, diagnostics_command("empty"));
    let call = receive(&mut socket).await;
    send(&mut socket, json!([3, call[1], {}])).await;
    let value = completed(submission).await;
    assert_eq!(value["lifecycle"]["accepted"], false);
    assert_eq!(value["diagnostics_16"]["job"]["state"], "no_log_available");
    // A native CALLERROR is kept as evidence.
    let submission = begin(&client, &fixture, diagnostics_command("refused"));
    let call = receive(&mut socket).await;
    send(
        &mut socket,
        json!([4, call[1], "NotImplemented", "no diagnostics", {}]),
    )
    .await;
    let value = completed(submission).await;
    assert_eq!(
        value["diagnostics_16"]["reply"],
        json!({"kind":"call_error","code":"NotImplemented"})
    );
    assert_eq!(value["diagnostics_16"]["job"]["state"], "rejected");
    // An active upload blocks another request before any byte is sent.
    let submission = begin(&client, &fixture, diagnostics_command("active"));
    let call = receive(&mut socket).await;
    send(&mut socket, json!([3, call[1], {"fileName":"a.log"}])).await;
    completed(submission).await;
    let (http, blocked) = submit_refused(&client, &fixture, diagnostics_command("blocked")).await;
    assert_eq!(http, 400);
    assert_eq!(blocked["lifecycle"]["stage"], "rejected");
    assert_eq!(blocked["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
    // The station claims success without uploading: the bridge does not believe it.
    notify(
        &mut socket,
        "claim",
        "DiagnosticsStatusNotification",
        json!({"status":"Uploaded"}),
    )
    .await;
    assert_eq!(
        job(&client, &fixture, "active").await["state"],
        "upload_unconfirmed"
    );
    // The Security Whitepaper notification exists only where GetLog is enabled.
    send(
        &mut socket,
        json!([2, "log-status", "LogStatusNotification", {"status":"Uploading","requestId":1}]),
    )
    .await;
    let reply: Value = receive(&mut socket).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "NotImplemented");
}
