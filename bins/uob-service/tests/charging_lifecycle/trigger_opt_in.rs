use super::*;

#[tokio::test]
async fn privileged_trigger_opt_in_advertises_only_rostered_station_and_connector() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for enabled in [false, true] {
        let fixture = Fixture::new();
        for (name, letter) in [("control-grant", "b"), ("privileged-grant", "c")] {
            let path = fixture.root.join(name);
            fs::write(&path, format!("uob1.demo.{}", letter.repeat(32))).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let document = fs::read_to_string(&fixture.configuration).unwrap();
        let document = document
            .replace(
                "[[charging.stations]]",
                &format!(
                    "control_grant_file='{}'\nprivileged_grant_file='{}'\n[[charging.stations]]",
                    fixture.root.join("control-grant").display(),
                    fixture.root.join("privileged-grant").display()
                ),
            )
            .replace(
                "credential_file='",
                if enabled {
                    "trigger_message=true\ncredential_file='"
                } else {
                    "credential_file='"
                },
            );
        fs::write(&fixture.configuration, document).unwrap();
        let mut child = fixture.start();
        ready(&mut child, fixture.management).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!(
            "http://127.0.0.1:{}/api/v1/command-schemas?station_id=station-a",
            fixture.management
        );
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        assert_eq!(
            client
                .get(&url)
                .bearer_auth(READ_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let mut websocket_request = request(fixture.charging);
        websocket_request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
        let (mut socket, _) = connect_async(websocket_request).await.unwrap();
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
        for token in ["b", "c"] {
            let response: serde_json::Value = client
                .get(&url)
                .bearer_auth(format!("uob1.demo.{}", token.repeat(32)))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let items = response["items"].as_array().unwrap();
            let triggers: Vec<_> = items
                .iter()
                .filter(|item| item["action"] == "TriggerMessage")
                .collect();
            assert_eq!(triggers.len(), if enabled { 2 } else { 0 });
            if enabled {
                assert!(
                    triggers
                        .iter()
                        .any(|item| item["resource"]["resource"].is_null())
                );
                assert!(triggers.iter().any(|item| {
                    item["resource"]["resource"]["kind"] == "connector"
                        && item["resource"]["resource"]["connector_id"] == "connector-1"
                        && item["resource"]["native_protocol_reference"]["protocol"] == "ocpp16"
                        && item["resource"]["native_protocol_reference"]["connector_id"] == 1
                }));
                assert!(triggers.iter().all(|item| {
                    item["resource"]["bridge_id"] == "bridge-1"
                        && item["resource"]["station_id"] == "station-a"
                }));
            }
        }
        stop(child);
    }
}
