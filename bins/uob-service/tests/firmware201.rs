#![cfg(unix)]
#[allow(dead_code, unused_imports)]
#[path = "firmware16/support.rs"]
mod firmware;
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "firmware201/recovery.rs"]
mod recovery;
#[path = "firmware201/refusals.rs"]
mod refusals;
#[allow(dead_code)]
#[path = "firmware201/support.rs"]
mod support;
use serde_json::json;
use support::*;

#[tokio::test]
async fn actual_daemon_runs_a_verifiable_secure_update_through_reboot_to_installed() {
    let (fixture, port) = fixture(true, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    assert!(advertised.contains("\"UpdateFirmware\""));
    assert!(!advertised.contains("SignedUpdateFirmware"));

    let (call, accepted) = answered(
        &client,
        &fixture,
        &mut socket,
        command("secure-1", 4242, SIGNED_IMAGE),
        json!({"status":"Accepted","statusInfo":{"reasonCode":"Ok","additionalInfo":"PRIVATE-NOTE"}}),
    )
    .await;
    assert_eq!(call[1], "secure-1");
    let request = &call[3];
    assert_eq!(request["requestId"], 4242);
    assert_eq!(request["retries"], 2);
    assert_eq!(request["retryInterval"], 30);
    let firmware = &request["firmware"];
    let location = format!("http://127.0.0.1:{port}/artifacts/{SIGNED_IMAGE}");
    assert_eq!(firmware["location"], location);
    assert_eq!(firmware["retrieveDateTime"], "2026-01-01T00:00:00Z");
    assert_eq!(firmware["installDateTime"], "2026-01-01T00:05:00Z");
    // L01.FR.11/L01.FR.04: the station can verify the signer and the whole image independently.
    assert!(station_accepts(&fixture, firmware, &image(2)));
    let mut tampered = image(2);
    tampered[0] ^= 1;
    assert!(!station_accepts(&fixture, firmware, &tampered));

    assert_eq!(accepted["schema_version"], json!({"major":1,"revision":15}));
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    let evidence = &accepted["firmware_201"];
    assert_eq!(evidence["request_id"], 4242);
    assert_eq!(evidence["secure"], true);
    assert_eq!(
        evidence["reply"],
        json!({"kind":"status","status":"Accepted","reason_code":"Ok"})
    );
    assert_eq!(evidence["job"]["state"], "accepted");
    assert_eq!(evidence["artifact"]["signed"], true);
    assert_private(&accepted);
    assert!(!accepted.to_string().contains("PRIVATE-NOTE"));
    download(&location, &evidence["artifact"], &image(2)).await;

    // Another update's identity never advances this job (L01.FR.10).
    notify(
        &mut socket,
        "foreign",
        json!({"status":"InvalidSignature","requestId":7}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "secure-1").await, "accepted");
    for (index, status) in [
        "DownloadScheduled",
        "Downloading",
        "DownloadPaused",
        "Downloading",
        "Downloaded",
        "SignatureVerified",
        "InstallScheduled",
        "InstallRebooting",
    ]
    .into_iter()
    .enumerate()
    {
        notify(
            &mut socket,
            &format!("status-{index}"),
            json!({"status":status,"requestId":4242}),
        )
        .await;
    }
    // A regression is counted and ignored.
    notify(
        &mut socket,
        "regression",
        json!({"status":"Downloading","requestId":4242}),
    )
    .await;
    let current = result(&client, &fixture, "secure-1").await;
    assert_eq!(current["firmware_201"]["job"]["state"], "install_rebooting");
    assert_eq!(current["firmware_201"]["job"]["rejected_transitions"], 1);
    // L01.FR.20: requestId may be omitted only for Idle.
    send(
        &mut socket,
        json!([2, "missing-id", "FirmwareStatusNotification", {"status":"Installed"}]),
    )
    .await;
    assert_eq!(receive(&mut socket).await[0], 4);

    // The new image reports success on a new socket (L01.FR.32 option a).
    drop(socket);
    let mut socket = connected(&fixture, &client).await;
    no_call(&mut socket).await;
    notify(
        &mut socket,
        "installed",
        json!({"status":"Installed","requestId":4242}),
    )
    .await;
    let installed = result(&client, &fixture, "secure-1").await;
    assert_eq!(installed["firmware_201"]["job"]["state"], "installed");
    assert_eq!(installed["firmware_201"]["job"]["notifications"], 10);
    assert_eq!(installed["firmware_201"]["reply"], evidence["reply"]);
    assert_eq!(installed["firmware_201"]["artifact"], evidence["artifact"]);
    notify(
        &mut socket,
        "late",
        json!({"status":"Downloading","requestId":4242}),
    )
    .await;
    assert_eq!(job_state(&client, &fixture, "secure-1").await, "installed");
}

#[tokio::test]
async fn non_secure_station_receives_no_signing_material_and_only_unsigned_images() {
    let (fixture, port) = fixture(false, false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let (http, refused) =
        submit_refused(&client, &fixture, command("signed-image", 1, SIGNED_IMAGE)).await;
    assert_eq!(http, 400);
    assert_eq!(refused["lifecycle"]["error"]["code"], "invalid_parameters");
    assert_eq!(
        job_state(&client, &fixture, "signed-image").await,
        "not_sent"
    );
    no_call(&mut socket).await;

    let (call, accepted) = answered(
        &client,
        &fixture,
        &mut socket,
        command("plain-1", 2, PLAIN_IMAGE),
        json!({"status":"Accepted"}),
    )
    .await;
    let location = format!("http://127.0.0.1:{port}/artifacts/{PLAIN_IMAGE}");
    assert_eq!(
        call[3]["firmware"],
        json!({"location":location,"retrieveDateTime":"2026-01-01T00:00:00Z","installDateTime":"2026-01-01T00:05:00Z"})
    );
    let evidence = &accepted["firmware_201"];
    assert_eq!(evidence["secure"], false);
    assert_eq!(evidence["artifact"]["signed"], false);
    download(&location, &evidence["artifact"], &image(1)).await;
    for (index, status) in ["Downloading", "Downloaded", "Installing", "Installed"]
        .into_iter()
        .enumerate()
    {
        notify(
            &mut socket,
            &format!("plain-{index}"),
            json!({"status":status,"requestId":2}),
        )
        .await;
        if index == 1 {
            // A signature report contradicts an update that carried no signature.
            notify(
                &mut socket,
                "contradiction",
                json!({"status":"SignatureVerified","requestId":2}),
            )
            .await;
        }
    }
    let installed = result(&client, &fixture, "plain-1").await;
    assert_eq!(installed["firmware_201"]["job"]["state"], "installed");
    assert_eq!(installed["firmware_201"]["job"]["rejected_transitions"], 1);
}
