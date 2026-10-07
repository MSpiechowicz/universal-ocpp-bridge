#![cfg(unix)]
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[allow(dead_code, unused_imports)]
#[path = "diagnostics16/support.rs"]
mod legacy;
#[path = "diagnostics201/recovery.rs"]
mod recovery;
#[path = "diagnostics201/refusals.rs"]
mod refusals;
#[allow(dead_code)]
#[path = "diagnostics201/support.rs"]
mod support;
use serde_json::json;
use support::*;

#[tokio::test]
async fn actual_daemon_confirms_a_log_upload_against_the_stored_file() {
    let (fixture, port) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    assert!(advertised.contains("\"GetLog\""));
    assert!(!advertised.contains("GetDiagnostics"));

    let submission = begin(&client, &fixture, command("log-1", "DiagnosticsLog", 4242));
    let call = receive(&mut socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[1], "log-1");
    assert_eq!(call[2], "GetLog");
    let location = location(&call);
    assert!(
        location.starts_with(&format!("http://127.0.0.1:{port}/uploads/")),
        "{location}"
    );
    // N01: the native request carries the bridge's destination and exactly the caller's bounds.
    assert_eq!(
        call[3],
        json!({"logType":"DiagnosticsLog","requestId":4242,"retries":2,"retryInterval":5,
            "log":{"remoteLocation":location,"oldestTimestamp":"2026-01-01T00:00:00Z",
                "latestTimestamp":"2026-01-02T00:00:00Z"}})
    );
    send(
        &mut socket,
        json!([3, call[1], {"status":"Accepted","filename":"diag-4242.log",
            "statusInfo":{"reasonCode":"Ok","additionalInfo":"PRIVATE-NOTE"}}]),
    )
    .await;
    let accepted = completed(submission).await;
    assert_eq!(accepted["schema_version"], json!({"major":1,"revision":17}));
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    let evidence = &accepted["diagnostics_201"];
    assert_eq!(evidence["log_type"], "DiagnosticsLog");
    assert_eq!(evidence["request_id"], 4242);
    assert_eq!(
        evidence["reply"],
        json!({"kind":"status","status":"Accepted","file_name":"diag-4242.log","reason_code":"Ok"})
    );
    assert_eq!(
        evidence["destination"],
        json!({"log_type":"DiagnosticsLog","maximum_bytes":65536,"test_only":true})
    );
    assert_eq!(evidence["job"]["state"], "accepted");
    assert_private(&accepted);
    assert!(!accepted.to_string().contains("PRIVATE-NOTE"));

    notify(
        &mut socket,
        "uploading",
        json!({"status":"Uploading","requestId":4242}),
    )
    .await;
    assert_eq!(job(&client, &fixture, "log-1").await["state"], "uploading");
    let bytes = log_bytes(3);
    assert_eq!(upload(&location, "diag-4242.log", &bytes).await, 201);
    notify(
        &mut socket,
        "uploaded",
        json!({"status":"Uploaded","requestId":4242}),
    )
    .await;
    let settled = result(&client, &fixture, "log-1").await;
    let job = &settled["diagnostics_201"]["job"];
    assert_eq!(job["state"], "uploaded");
    assert_eq!(job["notifications"], 2);
    assert_eq!(job["last_status"], "Uploaded");
    assert_eq!(
        job["upload"],
        json!({"sha256":sha256(&bytes),"size_bytes":bytes.len()})
    );
    assert_eq!(settled["diagnostics_201"]["reply"], evidence["reply"]);
    assert_private(&settled);
    // A late duplicate never reopens the settled job.
    notify(
        &mut socket,
        "late",
        json!({"status":"Uploading","requestId":4242}),
    )
    .await;
    assert_eq!(
        self::job(&client, &fixture, "log-1").await["state"],
        "uploaded"
    );
}

#[tokio::test]
async fn a_newer_request_cancels_the_older_upload_and_reports_match_only_their_request_id() {
    let (fixture, _) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let (call, first) = answered(
        &client,
        &fixture,
        &mut socket,
        command("first", "SecurityLog", 41),
        json!({"status":"Accepted","filename":"security-41.log"}),
    )
    .await;
    let first_location = location(&call);
    assert_eq!(first["diagnostics_201"]["log_type"], "SecurityLog");
    assert_eq!(
        first["diagnostics_201"]["destination"]["log_type"],
        "SecurityLog"
    );
    // Reports for another requestId never touch this job (N01.FR.07).
    notify(
        &mut socket,
        "other",
        json!({"status":"Uploading","requestId":99}),
    )
    .await;
    assert_eq!(job(&client, &fixture, "first").await["state"], "accepted");
    notify(
        &mut socket,
        "uploading",
        json!({"status":"Uploading","requestId":41}),
    )
    .await;
    assert_eq!(job(&client, &fixture, "first").await["state"], "uploading");

    // N01.FR.12: the station cancels the ongoing upload for a new request.
    let (call, second) = answered(
        &client,
        &fixture,
        &mut socket,
        command("second", "DiagnosticsLog", 42),
        json!({"status":"AcceptedCanceled","filename":"diag-42.log"}),
    )
    .await;
    let second_location = location(&call);
    assert_ne!(first_location, second_location);
    assert_eq!(second["lifecycle"]["accepted"], true);
    assert_eq!(
        second["diagnostics_201"]["reply"]["status"],
        "AcceptedCanceled"
    );
    assert_eq!(job(&client, &fixture, "first").await["state"], "cancelled");
    let bytes = log_bytes(9);
    assert_eq!(upload(&second_location, "diag-42.log", &bytes).await, 201);
    notify(
        &mut socket,
        "uploaded",
        json!({"status":"Uploaded","requestId":42}),
    )
    .await;
    let settled = job(&client, &fixture, "second").await;
    assert_eq!(settled["state"], "uploaded");
    assert_eq!(settled["upload"]["sha256"], sha256(&bytes));
    // N01.FR.20: the station's own AcceptedCanceled report is a counted late fact.
    notify(
        &mut socket,
        "cancelled",
        json!({"status":"AcceptedCanceled","requestId":41}),
    )
    .await;
    let cancelled = job(&client, &fixture, "first").await;
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["notifications"], 2);
    // A reused requestId is refused before the station sees it.
    let (http, _) = submit_refused(&client, &fixture, command("reuse", "SecurityLog", 42)).await;
    assert_eq!(http, 400);
    no_call(&mut socket).await;
}

#[tokio::test]
async fn a_station_that_cancels_its_own_upload_reports_it_natively() {
    let (fixture, _) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    answered(
        &client,
        &fixture,
        &mut socket,
        command("own", "SecurityLog", 7),
        json!({"status":"Accepted"}),
    )
    .await;
    notify(
        &mut socket,
        "cancel",
        json!({"status":"AcceptedCanceled","requestId":7}),
    )
    .await;
    let cancelled = job(&client, &fixture, "own").await;
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["last_status"], "AcceptedCanceled");
}
