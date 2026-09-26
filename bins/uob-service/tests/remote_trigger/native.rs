use super::*;

#[tokio::test]
async fn native_acceptance_is_not_observation_and_only_matching_committed_station_calls_count() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    verify_pre_boot_authorization(&fixture, &client).await;
    let mut station = fixture.station("station-a").await;
    let mut other = fixture.station("station-b").await;
    fixture.connected(&client, "station-a").await;
    fixture.connected(&client, "station-b").await;
    verify_boot_and_control_grant(&fixture, &client, &mut station, &mut other).await;
    verify_all_target_status(&fixture, &client, &mut station, &mut other).await;
    verify_all_target_observation(&fixture, &client, &mut station).await;
    verify_exact_status_scopes(&fixture, &client, &mut station).await;
    verify_other_classes_and_native_reject(&fixture, &client, &mut station).await;

    stop(child);
}

async fn verify_pre_boot_authorization(fixture: &Fixture, client: &reqwest::Client) {
    assert_eq!(
        client
            .post(fixture.url("/api/v1/commands"))
            .json(&command("unauthorized", "Heartbeat", None))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(READ)
            .json(&command("read-only", "Heartbeat", None))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

async fn verify_boot_and_control_grant(
    fixture: &Fixture,
    client: &reqwest::Client,
    station: &mut Socket,
    other: &mut Socket,
) {
    let boot = submit(
        client,
        fixture,
        station,
        "trigger-boot",
        "BootNotification",
        None,
        "Accepted",
    )
    .await;
    assert_eq!(boot["trigger_observation"]["status"], "pending");
    assert_eq!(
        station_call(
            station,
            "boot-a",
            "BootNotification",
            json!({"chargePointVendor":"A","chargePointModel":"A"})
        )
        .await[2]["status"],
        "Accepted"
    );
    observation(client, fixture, "trigger-boot", "observed").await;
    assert_eq!(
        client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(CONTROL)
            .json(&command("not-privileged", "Heartbeat", None))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        station_call(
            other,
            "boot-b",
            "BootNotification",
            json!({"chargePointVendor":"B","chargePointModel":"B"})
        )
        .await[2]["status"],
        "Accepted"
    );
}

async fn verify_all_target_status(
    fixture: &Fixture,
    client: &reqwest::Client,
    station: &mut Socket,
    other: &mut Socket,
) {
    let pending = submit(
        client,
        fixture,
        station,
        "trigger-status-all",
        "StatusNotification",
        None,
        "Accepted",
    )
    .await;
    assert_eq!(pending["trigger_observation"]["status"], "pending");
    station_call(
        other,
        "wrong-station",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    station_call(station, "wrong-class", "Heartbeat", json!({})).await;
    assert_eq!(
        station_call(
            station,
            "uncommitted-status",
            "StatusNotification",
            json!({"connectorId":99,"status":"Available","errorCode":"NoError"})
        )
        .await[0],
        4
    );
    assert_eq!(
        result(client, fixture, "trigger-status-all").await["trigger_observation"]["status"],
        "pending"
    );
}

async fn verify_all_target_observation(
    fixture: &Fixture,
    client: &reqwest::Client,
    station: &mut Socket,
) {
    station_call(
        station,
        "first-status",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    let partial = observation(client, fixture, "trigger-status-all", "partial").await;
    assert_eq!(
        partial["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    station_call(
        station,
        "duplicate-target",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    assert_eq!(
        result(client, fixture, "trigger-status-all").await["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    station_call(
        station,
        "second-status",
        "StatusNotification",
        json!({"connectorId":2,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    let partial = observation(client, fixture, "trigger-status-all", "partial").await;
    assert_eq!(
        partial["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    station_call(
        station,
        "station-status",
        "StatusNotification",
        json!({"connectorId":0,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    let observed = observation(client, fixture, "trigger-status-all", "observed").await;
    assert_eq!(
        observed["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

async fn verify_exact_status_scopes(
    fixture: &Fixture,
    client: &reqwest::Client,
    station: &mut Socket,
) {
    let station_only = submit(
        client,
        fixture,
        station,
        "trigger-status-zero",
        "StatusNotification",
        Some(0),
        "Accepted",
    )
    .await;
    assert_eq!(station_only["trigger_observation"]["status"], "pending");
    station_call(
        station,
        "wrong-connector-zero",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    assert_eq!(
        result(client, fixture, "trigger-status-zero").await["trigger_observation"]["status"],
        "pending"
    );
    station_call(
        station,
        "status-zero",
        "StatusNotification",
        json!({"connectorId":0,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    observation(client, fixture, "trigger-status-zero", "observed").await;

    let exact = submit(
        client,
        fixture,
        station,
        "trigger-status-one",
        "StatusNotification",
        Some(1),
        "Accepted",
    )
    .await;
    assert_eq!(exact["trigger_observation"]["status"], "pending");
    station_call(
        station,
        "wrong-connector-one",
        "StatusNotification",
        json!({"connectorId":2,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    assert_eq!(
        result(client, fixture, "trigger-status-one").await["trigger_observation"]["status"],
        "pending"
    );
    station_call(
        station,
        "status-one",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    observation(client, fixture, "trigger-status-one", "observed").await;

    let ignored = submit(
        client,
        fixture,
        station,
        "trigger-ignored-id",
        "Heartbeat",
        Some(999),
        "Accepted",
    )
    .await;
    assert_eq!(ignored["trigger_observation"]["status"], "pending");
    station_call(station, "ignored-id-heartbeat", "Heartbeat", json!({})).await;
    observation(client, fixture, "trigger-ignored-id", "observed").await;
}

async fn verify_other_classes_and_native_reject(
    fixture: &Fixture,
    client: &reqwest::Client,
    station: &mut Socket,
) {
    for (id, class, message, payload) in [
        ("trigger-heartbeat", "Heartbeat", "Heartbeat", json!({})),
        (
            "trigger-diagnostics",
            "DiagnosticsStatusNotification",
            "DiagnosticsStatusNotification",
            json!({"status":"Uploaded"}),
        ),
        (
            "trigger-firmware",
            "FirmwareStatusNotification",
            "FirmwareStatusNotification",
            json!({"status":"Installed"}),
        ),
        (
            "trigger-meter",
            "MeterValues",
            "MeterValues",
            json!({"connectorId":1,"meterValue":[{"timestamp":"2026-09-26T12:00:00Z","sampledValue":[{"value":"12.5"}]}]}),
        ),
    ] {
        let initial = submit(
            client,
            fixture,
            station,
            id,
            class,
            if class == "MeterValues" {
                Some(1)
            } else {
                None
            },
            "Accepted",
        )
        .await;
        assert_eq!(initial["trigger_observation"]["status"], "pending");
        assert_eq!(station_call(station, id, message, payload).await[0], 3);
        observation(client, fixture, id, "observed").await;
    }
    let rejected = submit(
        client,
        fixture,
        station,
        "trigger-rejected",
        "Heartbeat",
        None,
        "Rejected",
    )
    .await;
    assert_eq!(rejected["trigger_observation"]["status"], "unsupported");
}
