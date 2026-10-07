use super::support::*;
use serde_json::json;

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
        .json(&command("control-grant", 1, SIGNED_IMAGE))
        .send()
        .await
        .unwrap();
    assert!(unprivileged.status().is_client_error());
    let mut located = command("caller-location", 1, SIGNED_IMAGE);
    located["operation"]["parameters"]["payload"]["location"] = json!("http://attacker.invalid/x");
    let (http, _) = submit_refused(&client, &fixture, located).await;
    assert!((400..500).contains(&http));
    let mut evse = command("evse-scope", 1, SIGNED_IMAGE);
    evse["resource"]["resource"] = json!({"kind":"evse","evse_id":"one"});
    evse["resource"]["native_protocol_reference"] = json!({"protocol":"ocpp201","evse_id":1});
    let (http, _) = submit_refused(&client, &fixture, evse).await;
    assert!((400..500).contains(&http));
    for (id, request_id, reference, code) in [
        ("unknown-artifact", 2, "missing.bin", "policy_rejected"),
        // A secure station only receives signed images (L01.FR.11).
        ("unsigned-image", 3, PLAIN_IMAGE, "invalid_parameters"),
    ] {
        let (http, value) =
            submit_refused(&client, &fixture, command(id, request_id, reference)).await;
        assert_eq!(http, 400, "{id}");
        assert_eq!(value["lifecycle"]["error"]["code"], code, "{id}");
        assert_eq!(job_state(&client, &fixture, id).await, "not_sent", "{id}");
    }
    no_call(&mut socket).await;

    for (index, (reply, native)) in [
        (
            json!({"status":"InvalidCertificate"}),
            json!({"kind":"status","status":"InvalidCertificate"}),
        ),
        (
            json!({"status":"RevokedCertificate","statusInfo":{"reasonCode":"Revoked"}}),
            json!({"kind":"status","status":"RevokedCertificate","reason_code":"Revoked"}),
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
            command(&id, request_id, SIGNED_IMAGE),
            reply,
        )
        .await;
        assert_eq!(value["lifecycle"]["accepted"], false, "{id}");
        assert_eq!(value["firmware_201"]["reply"], native, "{id}");
        assert_eq!(value["firmware_201"]["job"]["state"], "rejected", "{id}");
    }
    let submission = begin(&client, &fixture, command("call-error", 20, SIGNED_IMAGE));
    let call = receive(&mut socket).await;
    send(
        &mut socket,
        json!([4, call[1], "NotImplemented", "no firmware", {}]),
    )
    .await;
    let value = completed(submission).await;
    assert_eq!(
        value["firmware_201"]["reply"],
        json!({"kind":"call_error","code":"NotImplemented"})
    );
    assert_eq!(value["firmware_201"]["job"]["state"], "rejected");
    // A retained requestId cannot be reused, since reports are matched by it alone.
    let (http, reused) =
        submit_refused(&client, &fixture, command("reused", 20, SIGNED_IMAGE)).await;
    assert_eq!(http, 400);
    assert_eq!(reused["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
}

#[tokio::test]
async fn accepted_canceled_ends_the_previous_update_and_late_reports_never_revive_it() {
    let (fixture, _) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    for (id, request_id, status) in [("first", 1, "Accepted"), ("second", 2, "AcceptedCanceled")] {
        let (_, value) = answered(
            &client,
            &fixture,
            &mut socket,
            command(id, request_id, SIGNED_IMAGE),
            json!({"status":status}),
        )
        .await;
        assert_eq!(value["lifecycle"]["accepted"], true);
        if id == "first" {
            notify(
                &mut socket,
                "first-installing",
                json!({"status":"Downloading","requestId":1}),
            )
            .await;
        }
    }
    assert_eq!(job_state(&client, &fixture, "first").await, "cancelled");
    assert_eq!(job_state(&client, &fixture, "second").await, "accepted");
    // L01.FR.24: the station may still report a failure for the cancelled update.
    notify(
        &mut socket,
        "late-failure",
        json!({"status":"DownloadFailed","requestId":1}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "first").await, "cancelled");
    notify(
        &mut socket,
        "second-progress",
        json!({"status":"Downloading","requestId":2}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "second").await, "downloading");
}
