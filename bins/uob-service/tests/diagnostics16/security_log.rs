use super::support::*;
use serde_json::json;

#[tokio::test]
async fn security_log_upload_matches_request_ids_and_a_newer_request_cancels_the_older() {
    let (fixture, _) = fixture(false, true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    assert!(advertised.contains("\"GetLog\""));
    assert!(!advertised.contains("\"GetDiagnostics\""));

    let submission = begin(&client, &fixture, log_command("first", "SecurityLog", 41));
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "GetLog");
    let first_location = call[3]["log"]["remoteLocation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        call[3],
        json!({"logType":"SecurityLog","requestId":41,"retryInterval":5,
            "log":{"remoteLocation":first_location,"oldestTimestamp":"2026-01-01T00:00:00Z"}})
    );
    send(
        &mut socket,
        json!([3, call[1], {"status":"Accepted","filename":"security-41.log"}]),
    )
    .await;
    let first = completed(submission).await;
    assert_eq!(first["diagnostics_16"]["action"], "GetLog");
    assert_eq!(first["diagnostics_16"]["log_type"], "SecurityLog");
    assert_eq!(first["diagnostics_16"]["request_id"], 41);
    assert_eq!(
        first["diagnostics_16"]["destination"]["log_type"],
        "SecurityLog"
    );
    // Reports for another requestId never touch this job (N01.FR.07).
    notify(
        &mut socket,
        "other",
        "LogStatusNotification",
        json!({"status":"Uploading","requestId":99}),
    )
    .await;
    assert_eq!(job(&client, &fixture, "first").await["state"], "accepted");
    notify(
        &mut socket,
        "uploading",
        "LogStatusNotification",
        json!({"status":"Uploading","requestId":41}),
    )
    .await;
    assert_eq!(job(&client, &fixture, "first").await["state"], "uploading");

    // N01.FR.11: the station cancels the ongoing upload for a new request.
    let submission = begin(
        &client,
        &fixture,
        log_command("second", "DiagnosticsLog", 42),
    );
    let call = receive(&mut socket).await;
    let second_location = call[3]["log"]["remoteLocation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(first_location, second_location);
    send(
        &mut socket,
        json!([3, call[1], {"status":"AcceptedCanceled","filename":"diag-42.log"}]),
    )
    .await;
    let second = completed(submission).await;
    assert_eq!(second["lifecycle"]["accepted"], true);
    assert_eq!(job(&client, &fixture, "first").await["state"], "cancelled");
    let bytes = log_bytes(9);
    assert_eq!(upload(&second_location, "diag-42.log", &bytes).await, 201);
    notify(
        &mut socket,
        "uploaded",
        "LogStatusNotification",
        json!({"status":"Uploaded","requestId":42}),
    )
    .await;
    let settled = job(&client, &fixture, "second").await;
    assert_eq!(settled["state"], "uploaded");
    assert_eq!(settled["upload"]["sha256"], sha256(&bytes));
    // A late report for the cancelled upload is counted but never revives it.
    notify(
        &mut socket,
        "late",
        "LogStatusNotification",
        json!({"status":"UploadFailure","requestId":41}),
    )
    .await;
    let cancelled = job(&client, &fixture, "first").await;
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["notifications"], 2);
    // A reused requestId is refused before the station sees it.
    let (http, _) =
        submit_refused(&client, &fixture, log_command("reuse", "SecurityLog", 42)).await;
    assert_eq!(http, 400);
    no_call(&mut socket).await;
}

#[tokio::test]
async fn oversized_uploads_are_refused_and_failure_statuses_are_native_evidence() {
    let (fixture, _) = fixture(false, true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    // An over-cap upload is refused by the destination and the failure is native evidence.
    let submission = begin(&client, &fixture, log_command("third", "SecurityLog", 43));
    let call = receive(&mut socket).await;
    let location = call[3]["log"]["remoteLocation"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    completed(submission).await;
    assert_eq!(upload(&location, "big.log", &vec![7_u8; 70_000]).await, 413);
    notify(
        &mut socket,
        "failure",
        "LogStatusNotification",
        json!({"status":"PermissionDenied","requestId":43}),
    )
    .await;
    assert_eq!(
        job(&client, &fixture, "third").await["state"],
        "permission_denied"
    );
}
