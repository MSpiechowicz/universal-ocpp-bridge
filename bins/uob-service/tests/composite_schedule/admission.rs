use super::support::*;
use serde_json::{Value, json};
use std::{fs, time::Duration};

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep admission denials and their no-wire evidence in one scenario.
async fn denies_invalid_scope_and_authority_before_wire() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    let mut disabled = fixture.station("station-b").await;
    fixture.connected(&client, "station-a").await;
    fixture.connected(&client, "station-b").await;

    for (credential, expected) in [(None, 401), (Some(READ), 401), (Some(CONTROL), 403)] {
        deny(
            &client,
            &fixture,
            &mut station,
            command("authority", 0, None),
            credential,
            expected,
        )
        .await;
    }
    let pre_boot = deny(
        &client,
        &fixture,
        &mut station,
        command("pre-boot", 0, None),
        Some(PRIVILEGED),
        400,
    )
    .await;
    assert_eq!(pre_boot["lifecycle"]["error"]["code"], "policy_rejected");
    boot(&mut station).await;
    boot(&mut disabled).await;

    verify_discovery(&client, &fixture).await;
    let mut off = command("default-off", 0, None);
    off["resource"]["station_id"] = json!("station-b");
    deny(&client, &fixture, &mut disabled, off, Some(PRIVILEGED), 422).await;
    no_call(&mut station).await;

    let invalid_payloads = [
        json!({"duration":120}),
        json!({"connectorId":0}),
        json!({"connectorId":null,"duration":120}),
        json!({"connectorId":0,"duration":null}),
        json!({"connectorId":0,"duration":0}),
        json!({"connectorId":0,"duration":-1}),
        json!({"connectorId":0,"duration":1.5}),
        json!({"connectorId":0,"duration":2_147_483_648_u64}),
        json!({"connectorId":-1,"duration":120}),
        json!({"connectorId":2_147_483_648_u64,"duration":120}),
        json!({"connectorId":0,"duration":120,"chargingRateUnit":"kW"}),
        json!({"connectorId":0,"duration":120,"chargingRateUnit":null}),
        json!({"connectorId":0,"duration":120,"chargingRateUnit":12}),
        json!({"connectorId":0,"duration":120,"extra":true}),
    ];
    for (index, payload) in invalid_payloads.into_iter().enumerate() {
        let mut body = command(&format!("invalid-{index}"), 0, None);
        body["operation"]["parameters"]["payload"] = payload;
        deny(&client, &fixture, &mut station, body, Some(PRIVILEGED), 400).await;
    }

    for (index, mutation, expected) in [
        (0, "schema", 400),
        (1, "protocol", 400),
        (2, "action", 400),
        (3, "station-widening", 400),
        (4, "connector-widening", 400),
        (5, "native-address", 400),
        (6, "canonical-address", 422),
        (7, "bridge", 403),
        (8, "station", 403),
        (9, "expired", 410),
    ] {
        let mut body = command(&format!("scope-{index}"), 1, Some("A"));
        match mutation {
            "schema" => body["operation"]["parameters"]["payload_schema"] = json!("wrong"),
            "protocol" => body["operation"]["parameters"]["protocol"] = json!("ocpp201"),
            "action" => body["operation"]["parameters"]["action"] = json!("UnknownAction"),
            "station-widening" => body["resource"] = command("unused", 0, None)["resource"].clone(),
            "connector-widening" => {
                body["operation"]["parameters"]["payload"]["connectorId"] = json!(0);
            }
            "native-address" => {
                body["resource"]["native_protocol_reference"]["connector_id"] = json!(2);
            }
            "canonical-address" => {
                body["resource"]["resource"]["connector_id"] = json!("unconfigured");
            }
            "bridge" => body["resource"]["bridge_id"] = json!("another-bridge"),
            "station" => body["resource"]["station_id"] = json!("unconfigured"),
            "expired" => body["expires_at"] = json!("2020-01-01T00:00:00Z"),
            _ => unreachable!(),
        }
        deny(
            &client,
            &fixture,
            &mut station,
            body,
            Some(PRIVILEGED),
            expected,
        )
        .await;
    }
    no_call(&mut disabled).await;
    drop(station);
    drop(disabled);
    drop(child);
    rejects_invalid_configuration(&fixture).await;
}

async fn deny(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    credential: Option<&str>,
    expected: u16,
) -> Value {
    let mut request = client.post(fixture.url("/api/v1/commands")).json(&body);
    if let Some(credential) = credential {
        request = request.bearer_auth(credential);
    }
    let response = request.send().await.unwrap();
    assert_eq!(
        response.status().as_u16(),
        expected,
        "denied request {}",
        body["request_id"]
    );
    let value = response.json().await.unwrap();
    no_call(socket).await;
    value
}

async fn verify_discovery(client: &reqwest::Client, fixture: &Fixture) {
    let response = client
        .get(fixture.url("/api/v1/command-schemas?station_id=station-a"))
        .bearer_auth(PRIVILEGED)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await.unwrap();
    let items = value["items"].as_array().unwrap();
    let expected = [
        command("grid", 0, None),
        command("one", 1, None),
        command("two", 2, None),
    ];
    assert_eq!(items.len(), expected.len());
    for body in expected {
        let descriptor = items
            .iter()
            .find(|item| item["resource"] == body["resource"])
            .unwrap();
        assert_eq!(descriptor["action"], "GetCompositeSchedule");
        assert_eq!(descriptor["protocol"], "ocpp16j");
        assert_eq!(descriptor["payload_schema"], SCHEMA);
        assert_eq!(descriptor["fields"][2]["enum_values"], json!(["A", "W"]));
    }
    let disabled: Value = client
        .get(fixture.url("/api/v1/command-schemas?station_id=station-b"))
        .bearer_auth(PRIVILEGED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(disabled["items"], json!([]));
}

async fn rejects_invalid_configuration(fixture: &Fixture) {
    let path = fixture.root.join("bridge.toml");
    let original = fs::read_to_string(&path).unwrap();
    let ocpp201 = original
        .replace("protocol='ocpp16j'", "protocol='ocpp201'")
        .replace(
            "connector_id='one'",
            "evse_id='evse-1'\nnative_evse_id=1\nconnector_id='one'",
        )
        .replace(
            "connector_id='two'",
            "evse_id='evse-2'\nnative_evse_id=2\nconnector_id='two'",
        );
    for config in [
        ocpp201,
        original.replace("native_connector_id=2", "native_connector_id=2147483648"),
        original.replace(
            &format!(
                "control_grant_file='{}'\n",
                fixture.root.join("control").display()
            ),
            "",
        ),
        original.replace(
            &format!(
                "privileged_grant_file='{}'\n",
                fixture.root.join("privileged").display()
            ),
            "",
        ),
    ] {
        fs::write(&path, config).unwrap();
        let mut process = fixture.start();
        process.expect_configuration_failure().await;
    }
    fs::write(path, original).unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
}
