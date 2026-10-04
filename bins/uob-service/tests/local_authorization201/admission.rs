use super::support::*;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};

#[tokio::test]
async fn station_only_privilege_default_off_and_reference_bindings_fail_before_wire() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let mut disabled = fixture.station("station-b").await;
    crate::host::boot(&mut disabled).await;
    fixture.connected(&client, "station-b").await;
    for mutation in 0..8 {
        let mut body = command(&format!("denied-{mutation}"), "SendLocalList", 1);
        let mut token = PRIVILEGED;
        match mutation {
            0 => token = CONTROL,
            1 => body["resource"]["station_id"] = json!("station-b"),
            2 => {
                body["resource"]["resource"] = json!({"kind":"evse","evse_id":"one"});
                body["resource"]["native_protocol_reference"] =
                    json!({"protocol":"ocpp201","evse_id":1});
            }
            3 => {
                body["operation"]["parameters"]["payload"]["localAuthorizationList"] =
                    json!([{"idTag":TOKEN}]);
            }
            4 => body["operation"]["parameters"]["payload"]["versionNumber"] = json!(2),
            5 => body["operation"]["parameters"]["payload"]["updateType"] = json!("Differential"),
            6 => body["operation"]["parameters"]["protocol"] = json!("ocpp16j"),
            _ => {
                body["operation"]["parameters"]["payload"]["updateReference"] =
                    json!(reference(999));
            }
        }
        let response = client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        no_call(&mut socket).await;
        no_call(&mut disabled).await;
        let lookup = client
            .get(fixture.url(&format!("/api/v1/commands/denied-{mutation}")))
            .bearer_auth(READ)
            .send()
            .await
            .unwrap();
        assert_eq!(lookup.status(), 404);
    }
    let descriptors = client
        .get(fixture.url("/api/v1/command-schemas?station_id=station-a"))
        .bearer_auth(PRIVILEGED)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    for action in ["GetLocalListVersion", "SendLocalList", "ClearCache"] {
        assert!(
            descriptors["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["action"] == action && item["resource"]["resource"].is_null())
        );
    }
    let descriptors = client
        .get(fixture.url("/api/v1/command-schemas?station_id=station-b"))
        .bearer_auth(PRIVILEGED)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(descriptors["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn protected_file_permissions_schema_semantics_and_protocol_guard_prevent_startup() {
    for mutation in 0..7 {
        let fixture = fixture();
        match mutation {
            0 => fs::set_permissions(
                fixture.root.join("updates.json"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap(),
            1 => {
                let path = fixture.root.join("updates.json");
                let target = fixture.root.join("updates-target.json");
                fs::rename(&path, &target).unwrap();
                std::os::unix::fs::symlink(target, path).unwrap();
            }
            2 => save(
                &fixture,
                &json!({"updates":[{"station_id":"station-a","update_reference":reference(1),"expires_at":"2099-01-01T00:00:00Z","request":{"versionNumber":1,"updateType":"Full","localAuthorizationList":[{"idTag":TOKEN}]}}]}),
            ),
            3 => {
                let path = fixture.root.join("bridge.toml");
                fs::write(
                    &path,
                    fs::read_to_string(&path).unwrap().replacen(
                        "protocol='ocpp201'",
                        "protocol='ocpp16j'",
                        1,
                    ),
                )
                .unwrap();
            }
            4 => {
                let mut request = native(1);
                request["localAuthorizationList"][0]["idToken"]["idToken"] =
                    json!("PRIVATE-nonASCII-ż");
                save(
                    &fixture,
                    &json!({"updates":[{"station_id":"station-a","update_reference":reference(1),
                    "expires_at":"2099-01-01T00:00:00Z","request":request}]}),
                );
            }
            5 => {
                let mut request = native(1);
                request["localAuthorizationList"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("idTokenInfo");
                save(
                    &fixture,
                    &json!({"updates":[{"station_id":"station-a","update_reference":reference(1),
                    "expires_at":"2099-01-01T00:00:00Z","request":request}]}),
                );
            }
            _ => {
                let path = fixture.root.join("bridge.toml");
                let config = fs::read_to_string(&path).unwrap();
                let line = format!(
                    "privileged_grant_file='{}'\n",
                    fixture.root.join("privileged").display()
                );
                fs::write(path, config.replace(&line, "")).unwrap();
            }
        }
        fixture.start().expect_configuration_failure().await;
    }
}
