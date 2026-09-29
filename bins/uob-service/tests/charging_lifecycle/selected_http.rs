use super::*;

#[tokio::test]
async fn selected_http_target_serves_the_canonical_charging_snapshot() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = Fixture::new();
    let integration = vacant_port();
    let credential = fixture.root.join("ems-credentials.toml");
    fs::write(
        &credential,
        "[[principals]]\nid='ems-reader'\ntoken='private-integration-test-token'\n\
         permissions=['read']\nbridges=['bridge-1']\n",
    )
    .unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    let config = fs::read_to_string(&fixture.configuration).unwrap().replace(
        "environment='demo'",
        "environment='demo'\ntarget_id='ems-main'",
    );
    fs::write(
        &fixture.configuration,
        format!(
            "{config}\n[[targets]]\nid='ems-main'\nkind='ems-scada.http'\n\
             enabled=true\nrevision=1\n[targets.settings]\n\
             listen_addr='127.0.0.1:{integration}'\ncredentials_file='{}'\n",
            credential.display()
        ),
    )
    .unwrap();

    let mut child = fixture.start();
    ready(&mut child, integration).await;
    ready(&mut child, fixture.charging).await;
    let (mut socket, _) = connect_async(request(fixture.charging)).await.unwrap();
    assert_eq!(
        call(
            &mut socket,
            serde_json::json!([2, "boot", "BootNotification", {
                "chargePointVendor": "LiveVendor", "chargePointModel": "LiveModel"
            }])
        )
        .await[2]["status"],
        "Accepted"
    );
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let management = detail(&client, fixture.management).await;
    let response = client
        .get(format!(
            "http://127.0.0.1:{integration}/bridge/v1/stations/station-a"
        ))
        .bearer_auth("private-integration-test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let target: serde_json::Value = response.json().await.unwrap();
    assert_eq!(target["station"], management["station"]);
    assert_eq!(target["connectivity"], management["connectivity"]);
    assert_eq!(target["resources"], management["resources"]);
    stop(child);
}
