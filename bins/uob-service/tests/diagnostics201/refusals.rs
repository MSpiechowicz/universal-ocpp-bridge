use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn refusals_never_reach_the_station_and_native_refusals_are_evidence() {
    let (fixture, _) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let unprivileged = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&command("control-grant", "SecurityLog", 1))
        .send()
        .await
        .unwrap();
    assert!(unprivileged.status().is_client_error());
    let mut located = command("caller-location", "SecurityLog", 1);
    located["operation"]["parameters"]["payload"]["log"] =
        json!({"remoteLocation":"http://attacker.invalid/x"});
    let (http, _) = submit_refused(&client, &fixture, located).await;
    assert!((400..500).contains(&http));
    let mut whitepaper = command("whitepaper-schema", "SecurityLog", 1);
    whitepaper["operation"]["parameters"]["payload_schema"] =
        json!("urn:uob:ocpp16:GetLogReference:1");
    let (http, _) = submit_refused(&client, &fixture, whitepaper).await;
    assert!((400..500).contains(&http));
    let mut evse = command("evse-scope", "SecurityLog", 1);
    evse["resource"]["resource"] = json!({"kind":"evse","evse_id":"one"});
    evse["resource"]["native_protocol_reference"] = json!({"protocol":"ocpp201","evse_id":1});
    let (http, _) = submit_refused(&client, &fixture, evse).await;
    assert!((400..500).contains(&http));
    let mut inverted = command("inverted-window", "SecurityLog", 1);
    inverted["operation"]["parameters"]["payload"]["latestTimestamp"] =
        json!("2025-01-01T00:00:00Z");
    let (http, _) = submit_refused(&client, &fixture, inverted).await;
    assert!((400..500).contains(&http));
    no_call(&mut socket).await;

    for (index, (reply, native)) in [
        (
            json!({"status":"Rejected","statusInfo":{"reasonCode":"NoLogs"}}),
            json!({"kind":"status","status":"Rejected","reason_code":"NoLogs"}),
        ),
        (
            json!({"status":"Rejected"}),
            json!({"kind":"status","status":"Rejected"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("refused-{index}");
        let request_id = 10 + i32::try_from(index).unwrap();
        let (_, value) = answered(
            &client,
            &fixture,
            &mut socket,
            command(&id, "DiagnosticsLog", request_id),
            reply,
        )
        .await;
        // N01.FR.05: Rejected is a protocol rejection and nothing will be uploaded.
        assert_eq!(value["lifecycle"]["accepted"], false, "{id}");
        assert_eq!(value["lifecycle"]["error"]["code"], "protocol_rejected");
        assert_eq!(value["diagnostics_201"]["reply"], native, "{id}");
        assert_eq!(value["diagnostics_201"]["job"]["state"], "rejected", "{id}");
    }
    let submission = begin(&client, &fixture, command("call-error", "SecurityLog", 20));
    let call = receive(&mut socket).await;
    send(
        &mut socket,
        json!([4, call[1], "NotImplemented", "no logs", {}]),
    )
    .await;
    let value = completed(submission).await;
    assert_eq!(
        value["diagnostics_201"]["reply"],
        json!({"kind":"call_error","code":"NotImplemented"})
    );
    assert_eq!(value["diagnostics_201"]["job"]["state"], "rejected");
    // A retained requestId cannot be reused, since reports are matched by it alone.
    let (http, reused) =
        submit_refused(&client, &fixture, command("reused", "SecurityLog", 20)).await;
    assert_eq!(http, 400);
    assert_eq!(reused["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
}

#[tokio::test]
async fn unverified_claims_and_failures_are_explicit_and_unlisted_stations_refuse_logs() {
    let (fixture, _) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    // The station claims success without uploading: the bridge does not believe it.
    let (_, _) = answered(
        &client,
        &fixture,
        &mut socket,
        command("claim", "DiagnosticsLog", 30),
        json!({"status":"Accepted","filename":"claim.log"}),
    )
    .await;
    notify(
        &mut socket,
        "claim",
        json!({"status":"Uploaded","requestId":30}),
    )
    .await;
    let claim = job(&client, &fixture, "claim").await;
    assert_eq!(claim["state"], "upload_unconfirmed");
    assert!(claim["upload"].is_null());
    // An over-cap upload is refused by the destination and the failure is native evidence.
    let (call, _) = answered(
        &client,
        &fixture,
        &mut socket,
        command("big", "SecurityLog", 31),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(
        upload(&location(&call), "big.log", &vec![7_u8; 70_000]).await,
        413
    );
    notify(
        &mut socket,
        "failure",
        json!({"status":"UploadFailure","requestId":31}),
    )
    .await;
    assert_eq!(
        job(&client, &fixture, "big").await["state"],
        "upload_failed"
    );
    for (index, (status, state)) in [
        ("BadMessage", "bad_message"),
        ("PermissionDenied", "permission_denied"),
        ("NotSupportedOperation", "not_supported_operation"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("native-{index}");
        let request_id = 40 + i32::try_from(index).unwrap();
        answered(
            &client,
            &fixture,
            &mut socket,
            command(&id, "SecurityLog", request_id),
            json!({"status":"Accepted"}),
        )
        .await;
        notify(
            &mut socket,
            &id,
            json!({"status":status,"requestId":request_id}),
        )
        .await;
        assert_eq!(job(&client, &fixture, &id).await["state"], state);
    }
    // N01.FR.13: an identity-free Idle is acceptable only as the answer to a trigger.
    send(
        &mut socket,
        json!([2, "spontaneous", "LogStatusNotification", {"status":"Idle"}]),
    )
    .await;
    let reply: Value = receive(&mut socket).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "OccurrenceConstraintViolation");
    // A non-Idle report without its upload identity never reaches the application.
    send(
        &mut socket,
        json!([2, "anonymous", "LogStatusNotification", {"status":"Uploading"}]),
    )
    .await;
    let reply: Value = receive(&mut socket).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "PropertyConstraintViolation");
}

#[tokio::test]
async fn a_station_without_the_opt_in_refuses_get_log_and_its_notification() {
    let fixture = fixture_without_logs();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    assert!(!advertised.contains("\"GetLog\""));
    // Not enabled on this station: refused before durable admission.
    let (http, _) = submit_refused(&client, &fixture, command("log-off", "SecurityLog", 1)).await;
    assert_eq!(http, 422);
    no_call(&mut socket).await;
    // The notification exists only where the workflow was explicitly enabled.
    send(
        &mut socket,
        json!([2, "log-status", "LogStatusNotification", {"status":"Uploading","requestId":1}]),
    )
    .await;
    let reply: Value = receive(&mut socket).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "NotImplemented");
}
