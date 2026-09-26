use super::*;

#[tokio::test]
async fn disconnected_trigger_becomes_absent_and_restart_never_resends_it() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut station = fixture.station("station-a").await;
    station_call(
        &mut station,
        "boot",
        "BootNotification",
        json!({"chargePointVendor":"A","chargePointModel":"A"}),
    )
    .await;
    let pending = submit(
        &client,
        &fixture,
        &mut station,
        "trigger-absent",
        "Heartbeat",
        None,
        "Accepted",
    )
    .await;
    assert_eq!(pending["trigger_observation"]["status"], "pending");
    submit(
        &client,
        &fixture,
        &mut station,
        "trigger-partial",
        "StatusNotification",
        None,
        "Accepted",
    )
    .await;
    station_call(
        &mut station,
        "one-of-three",
        "StatusNotification",
        json!({"connectorId":1,"status":"Available","errorCode":"NoError"}),
    )
    .await;
    observation(&client, &fixture, "trigger-partial", "partial").await;
    drop(station);
    stop(child);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let status = tokio::time::timeout(Duration::from_secs(70), async {
        loop {
            let value = result(&client, &fixture, "trigger-absent").await;
            if value["trigger_observation"]["status"] == "absent" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("disconnected deadline sweep");
    assert!(
        status["trigger_observation"]["observed"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let partial = result(&client, &fixture, "trigger-partial").await;
    assert_eq!(partial["trigger_observation"]["status"], "partial");
    assert_eq!(
        partial["trigger_observation"]["observed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut reconnected = fixture.station("station-a").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(400), reconnected.next())
            .await
            .is_err(),
        "reconnect must not retransmit an admitted trigger"
    );
    station_call(
        &mut reconnected,
        "reboot",
        "BootNotification",
        json!({"chargePointVendor":"A","chargePointModel":"A"}),
    )
    .await;
    assert_eq!(
        result(&client, &fixture, "trigger-absent").await["trigger_observation"]["status"],
        "absent"
    );
    stop(child);
}
