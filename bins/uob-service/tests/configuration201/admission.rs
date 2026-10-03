use super::support::*;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

async fn denied(client: &reqwest::Client, fixture: &Fixture, body: &Value, token: &str) -> Value {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let response: Value = response.json().await.unwrap();
    assert!(status.is_client_error(), "{status}: {response}");
    let durable = client
        .get(fixture.url(&format!(
            "/api/v1/commands/{}",
            body["request_id"].as_str().unwrap()
        )))
        .bearer_auth(crate::host::READ)
        .send()
        .await
        .unwrap();
    assert_eq!(
        durable.status(),
        404,
        "invalid protected command must not be admitted"
    );
    assert!(!response.to_string().contains(SECRET));
    response
}

#[tokio::test]
async fn grants_defaultoff_raw_schema_scope_protocol_expiry_and_binding_fail_closed() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    let mut off = fixture.station_protocol("station-b", "ocpp2.0.1").await;
    boot(&mut station).await;
    boot(&mut off).await;
    let grant = variables("wrong-grant", vec![entry(9, "Password")]);
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&grant)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    no_call(&mut station).await;
    for mut body in [
        variables("off-variable", vec![entry(9, "Password")]),
        network("off-network", 0),
    ] {
        body["resource"]["station_id"] = json!("station-b");
        denied(&client, &fixture, &body, PRIVILEGED).await;
        no_call(&mut off).await;
    }
    let mut cases = Vec::new();
    let mut raw = variables("raw", vec![entry(9, "Password")]);
    raw["operation"]["parameters"]["payload"]["setVariableData"][0]["attributeValue"] =
        json!(SECRET);
    cases.push(raw);
    let mut raw_network = network("raw-network", 0);
    raw_network["operation"]["parameters"]["payload"]["connectionData"] =
        profile("wss://secret.invalid");
    cases.push(raw_network);
    let mut schema = variables("wrong-schema", vec![entry(9, "Password")]);
    schema["operation"]["parameters"]["payload_schema"] =
        json!("urn:OCPP:Cp:2:2020:3:SetVariablesRequest");
    cases.push(schema);
    let mut protocol = variables("wrong-protocol", vec![entry(9, "Password")]);
    protocol["operation"]["parameters"]["protocol"] = json!("ocpp16j");
    cases.push(protocol);
    cases.push(variables("expired", vec![entry(10, "Expired")]));
    cases.push(variables("wrong-identity", vec![entry(9, "OtherPassword")]));
    cases.push(variables("missing-reference", vec![entry(99, "Password")]));
    let mut slot = network("wrong-slot", 0);
    slot["operation"]["parameters"]["payload"]["configurationSlot"] = json!(3);
    cases.push(slot);
    let mut child_network = network("child-network", 0);
    child_network["resource"] = json!({"bridge_id":"bridge-1","station_id":"station-a", "resource":{"kind":"evse","evse_id":"evse-one","connector_id":"one"},"native_protocol_reference":{"protocol":"ocpp201","evse_id":1,"connector_id":1}});
    cases.push(child_network);
    let mut child_variables = variables("child-binding", vec![entry(9, "Password")]);
    child_variables["resource"] = json!({"bridge_id":"bridge-1","station_id":"station-a", "resource":{"kind":"evse","evse_id":"evse-one","connector_id":"one"},"native_protocol_reference":{"protocol":"ocpp201","evse_id":1,"connector_id":1}});
    cases.push(child_variables);
    for body in cases {
        denied(&client, &fixture, &body, PRIVILEGED).await;
        no_call(&mut station).await;
    }
}

#[tokio::test]
async fn private_startup_checks_reject_permissions_symlinks_hardlinks_overlap_and_bounds() {
    for scenario in [
        "permissions",
        "symlink",
        "hardlink",
        "grant-overlap",
        "state-overlap",
        "file-size",
        "unicode",
        "profile",
        "entries",
        "wrong-resource",
        "wrong-native",
        "disabled-variable",
        "disabled-network",
        "protocol",
        "missing-grant",
        "missing-file",
        "unknown-field",
    ] {
        let fixture = fixture();
        let path = fixture.root.join("configuration201.json");
        let mut data = provisioning();
        let config_path = fixture.root.join("bridge.toml");
        let mut config = fs::read_to_string(&config_path).unwrap();
        match scenario {
            "permissions" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            "symlink" => {
                fs::rename(&path, fixture.root.join("actual.json")).unwrap();
                symlink(fixture.root.join("actual.json"), &path).unwrap();
            }
            "hardlink" => fs::hard_link(&path, fixture.root.join("alias.json")).unwrap(),
            "grant-overlap" => {
                config = config.replace(
                    &path.display().to_string(),
                    &fixture.root.join("read").display().to_string(),
                );
            }
            "state-overlap" => {
                config = config.replace(
                    &path.display().to_string(),
                    &fixture.root.join("state/values.json").display().to_string(),
                );
            }
            "file-size" => fs::write(&path, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap(),
            "unicode" => data["variables"][0]["value"] = json!("🦀".repeat(1001)),
            "profile" => data["network_profiles"][0]["profile"]["ocppVersion"] = json!("NOT-OCPP"),
            "entries" => data["variables"] = json!(vec![data["variables"][0].clone(); 129]),
            "wrong-resource" => {
                data["variables"][0]["resource"]["bridge_id"] = json!("other-bridge");
            }
            "wrong-native" => {
                data["variables"][0]["resource"]["native_protocol_reference"] =
                    json!({"protocol":"ocpp201","evse_id":999});
            }
            "disabled-variable" => {
                config = config.replace("set_variables=true", "set_variables=false");
            }
            "disabled-network" => {
                config = config.replace("set_network_profile=true", "set_network_profile=false");
            }
            "protocol" => config = config.replace("protocol='ocpp201'", "protocol='ocpp16j'"),
            "missing-grant" => {
                config = config
                    .lines()
                    .filter(|line| !line.starts_with("privileged_grant_file="))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            "missing-file" => {
                config = config
                    .lines()
                    .filter(|line| !line.starts_with("configuration_values_file="))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            "unknown-field" => data["variables"][0]["raw_extra"] = json!(SECRET),
            _ => unreachable!(),
        }
        if [
            "unicode",
            "profile",
            "entries",
            "wrong-resource",
            "wrong-native",
            "unknown-field",
        ]
        .contains(&scenario)
        {
            save_provisioning(&fixture, &data);
        }
        fs::write(config_path, config).unwrap();
        fixture.start().expect_configuration_failure().await;
    }
}

#[tokio::test]
async fn independent_action_optins_do_not_enable_the_other_write_or_queries() {
    for (enabled, disabled) in [
        ("SetVariables", "SetNetworkProfile"),
        ("SetNetworkProfile", "SetVariables"),
    ] {
        let fixture = fixture();
        let path = fixture.root.join("bridge.toml");
        let key = if disabled == "SetVariables" {
            "set_variables"
        } else {
            "set_network_profile"
        };
        let config = fs::read_to_string(&path)
            .unwrap()
            .replace(&format!("{key}=true"), &format!("{key}=false"))
            .replace("get_variables=true", "get_variables=false");
        fs::write(path, config).unwrap();
        let mut data = provisioning();
        data[if disabled == "SetVariables" {
            "variables"
        } else {
            "network_profiles"
        }] = json!([]);
        save_provisioning(&fixture, &data);
        let mut child = fixture.start();
        fixture.ready(&mut child).await;
        let client = client();
        let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
        boot(&mut station).await;
        let body = if disabled == "SetVariables" {
            variables("disabled", vec![entry(1, "Empty")])
        } else {
            network("disabled", 0)
        };
        denied(&client, &fixture, &body, PRIVILEGED).await;
        denied(&client,&fixture,&command("query-off","GetVariables",json!({"getVariableData":[{"component":{"name":"DeviceDataCtrlr"},"variable":{"name":"ItemsPerMessage","instance":"SetVariables"}}]})),PRIVILEGED).await;
        no_call(&mut station).await;
        let body = if enabled == "SetVariables" {
            variables("enabled", vec![entry(1, "Empty")])
        } else {
            network("enabled", 0)
        };
        let request = begin(&client, &fixture, body);
        let call = receive(&mut station).await;
        assert_eq!(call[2], enabled);
        let mut peer = Peer::default();
        let reply = if enabled == "SetVariables" {
            peer.set_variables(&call[3])
        } else {
            peer.set_network(&call[3])
        };
        send(&mut station, json!([3, call[1], reply])).await;
        assert_eq!(completed(request).await["lifecycle"]["accepted"], true);
    }
}
