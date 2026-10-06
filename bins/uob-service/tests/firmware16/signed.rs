use super::support::*;
use serde_json::json;

#[tokio::test]
async fn actual_daemon_sends_verifiable_signed_firmware_and_matches_request_ids() {
    let (fixture, port) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    assert!(advertised.contains("\"SignedUpdateFirmware\""));
    assert!(!advertised.contains("\"UpdateFirmware\""));

    let submission = begin(
        &client,
        &fixture,
        signed_command("signed-1", 4242, SIGNED_IMAGE),
    );
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "SignedUpdateFirmware");
    let request = &call[3];
    assert_eq!(request["requestId"], 4242);
    let firmware = &request["firmware"];
    let location = format!("http://127.0.0.1:{port}/artifacts/{SIGNED_IMAGE}");
    assert_eq!(firmware["location"], location);
    assert_eq!(firmware["retrieveDateTime"], "2026-01-01T00:00:00Z");
    assert_eq!(firmware["installDateTime"], "2026-01-01T00:05:00Z");
    assert!(
        firmware["signingCertificate"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert!(station_accepts(&fixture, firmware, &image(2)));
    let mut tampered = image(2);
    tampered[0] ^= 1;
    assert!(!station_accepts(&fixture, firmware, &tampered));
    send(&mut socket, json!([3, call[1], {"status":"Accepted"}])).await;
    let accepted = completed(submission).await;
    let evidence = &accepted["firmware_16"];
    assert_eq!(evidence["action"], "SignedUpdateFirmware");
    assert_eq!(evidence["request_id"], 4242);
    assert_eq!(
        evidence["reply"],
        json!({"kind":"status","status":"Accepted"})
    );
    assert_eq!(evidence["artifact"]["signed"], true);
    for marker in ["CERTIFICATE", "signature", "http://"] {
        assert!(!accepted.to_string().contains(marker), "{marker}");
    }
    download(&location, &evidence["artifact"], &image(2)).await;

    // Another update's identity never advances this job (L01.FR.10).
    notify(
        &mut socket,
        "foreign",
        "SignedFirmwareStatusNotification",
        json!({"status":"InvalidSignature","requestId":7}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "signed-1").await, "accepted");
    let sequence = [
        "DownloadScheduled",
        "Downloading",
        "Downloaded",
        "SignatureVerified",
        "InstallScheduled",
        "InstallRebooting",
    ];
    for (index, status) in sequence.into_iter().enumerate() {
        notify(
            &mut socket,
            &format!("signed-status-{index}"),
            "SignedFirmwareStatusNotification",
            json!({"status":status,"requestId":4242}),
        )
        .await;
    }
    assert_eq!(
        job_state(&client, &fixture, "signed-1").await,
        "install_rebooting"
    );
    // A regression is counted and ignored.
    notify(
        &mut socket,
        "regression",
        "SignedFirmwareStatusNotification",
        json!({"status":"Downloading","requestId":4242}),
    )
    .await;
    let current = result(&client, &fixture, "signed-1").await;
    assert_eq!(current["firmware_16"]["job"]["state"], "install_rebooting");
    assert_eq!(current["firmware_16"]["job"]["rejected_transitions"], 1);
    // L01.FR.21: requestId may be omitted only for Idle.
    send(
        &mut socket,
        json!([2, "missing-id", "SignedFirmwareStatusNotification", {"status":"Installed"}]),
    )
    .await;
    assert_eq!(receive(&mut socket).await[0], 4);
    drop(socket);
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    notify(
        &mut socket,
        "installed",
        "SignedFirmwareStatusNotification",
        json!({"status":"Installed","requestId":4242}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "signed-1").await, "installed");

    // A refused certificate is native evidence and releases nothing physical.
    let submission = begin(
        &client,
        &fixture,
        signed_command("signed-2", 4243, SIGNED_IMAGE),
    );
    let call = receive(&mut socket).await;
    send(
        &mut socket,
        json!([3, call[1], {"status":"InvalidCertificate"}]),
    )
    .await;
    let refused = completed(submission).await;
    assert_eq!(refused["lifecycle"]["accepted"], false);
    assert_eq!(refused["firmware_16"]["job"]["state"], "rejected");
    // Reusing a retained requestId is refused before transmission.
    let (http, reused) = submit_refused(
        &client,
        &fixture,
        signed_command("signed-3", 4242, SIGNED_IMAGE),
    )
    .await;
    assert_eq!(http, 400);
    assert_eq!(reused["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
}

#[tokio::test]
async fn accepted_canceled_ends_the_previous_signed_job() {
    let (fixture, _) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    for (id, request_id, status) in [("first", 1, "Accepted"), ("second", 2, "AcceptedCanceled")] {
        let submission = begin(
            &client,
            &fixture,
            signed_command(id, request_id, SIGNED_IMAGE),
        );
        let call = receive(&mut socket).await;
        send(&mut socket, json!([3, call[1], {"status":status}])).await;
        assert_eq!(completed(submission).await["lifecycle"]["accepted"], true);
    }
    assert_eq!(job_state(&client, &fixture, "first").await, "cancelled");
    assert_eq!(job_state(&client, &fixture, "second").await, "accepted");
    // The station may still report a failure for the cancelled update (L01.FR.26).
    notify(
        &mut socket,
        "late-failure",
        "SignedFirmwareStatusNotification",
        json!({"status":"DownloadFailed","requestId":1}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "first").await, "cancelled");
}
