use super::support::*;
use serde_json::{Value, json};
use std::fs;

#[tokio::test]
async fn privilege_registration_precision_and_id_clear_authority_deny_without_call() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    fixture.connected(&client, "station-a").await;
    for (token, status) in [(READ, 401), (CONTROL, 403), (PRIVILEGED, 400)] {
        deny(
            &client,
            &fixture,
            &mut station,
            command(
                &format!("authority-{status}"),
                0,
                "ClearChargingProfile",
                json!({}),
            ),
            token,
            status,
        )
        .await;
    }
    boot(&mut station).await;
    let mut disabled = fixture.station("station-b").await;
    boot(&mut disabled).await;
    for action in ["SetChargingProfile", "ClearChargingProfile"] {
        let payload = if action == "SetChargingProfile" {
            profile(0, 117, "0", "A")
        } else {
            json!({})
        };
        let mut body = command(&format!("disabled-{action}"), 0, action, payload);
        body["resource"]["station_id"] = json!("station-b");
        deny(&client, &fixture, &mut disabled, body, PRIVILEGED, 422).await;
    }
    deny_invalid_registered_profiles(&client, &fixture, &mut station).await;
    let discovery: Value = client
        .get(fixture.url("/api/v1/command-schemas?station_id=station-a"))
        .bearer_auth(PRIVILEGED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for action in ["SetChargingProfile", "ClearChargingProfile"] {
        assert!(
            discovery["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|descriptor| descriptor["action"] == action
                    && descriptor["resource"]["resource"]["connector_id"] == "one")
        );
    }
    no_call(&mut station).await;
}

async fn deny_invalid_registered_profiles(
    client: &reqwest::Client,
    fixture: &Fixture,
    station: &mut Socket,
) {
    for (index, payload) in [
        json!({"id":117,"connectorId":1}),
        json!({}),
        json!({"connectorId":2}),
        json!({"connectorId":0}),
    ]
    .into_iter()
    .enumerate()
    {
        deny(
            client,
            fixture,
            station,
            command(
                &format!("invalid-clear-{index}"),
                1,
                "ClearChargingProfile",
                payload,
            ),
            PRIVILEGED,
            400,
        )
        .await;
    }
    for (index, rate) in ["1.01", "-0.1", "79228162514264337593543950335.1"]
        .into_iter()
        .enumerate()
    {
        deny(
            client,
            fixture,
            station,
            command(
                &format!("invalid-rate-{index}"),
                1,
                "SetChargingProfile",
                profile(1, 117, rate, "W"),
            ),
            PRIVILEGED,
            400,
        )
        .await;
    }
    let mut tx = profile(1, 117, "0", "A");
    tx["csChargingProfiles"]["chargingProfilePurpose"] = json!("TxProfile");
    tx["csChargingProfiles"]["transactionId"] = json!(117);
    deny(
        client,
        fixture,
        station,
        command("missing-live-tx", 1, "SetChargingProfile", tx),
        PRIVILEGED,
        400,
    )
    .await;
}

async fn deny(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    token: &str,
    status: u16,
) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        status,
        "denied {}",
        body["request_id"]
    );
    no_call(socket).await;
}

#[tokio::test]
async fn native_profile_flags_require_independent_grants_supported_protocol_and_signed_address() {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let original = fs::read_to_string(&path).unwrap();
    for configuration in [
        original.replace("native_connector_id=2", "native_connector_id=2147483648"),
        original.replace(
            &format!(
                "privileged_grant_file='{}'\n",
                fixture.root.join("privileged").display()
            ),
            "",
        ),
        original.replace(
            &format!(
                "control_grant_file='{}'\n",
                fixture.root.join("control").display()
            ),
            "",
        ),
        original
            .replace("protocol='ocpp16j'", "protocol='ocpp201'")
            .replace(
                "connector_id='one'",
                "evse_id='one'\nnative_evse_id=1\nconnector_id='one'",
            )
            .replace(
                "connector_id='two'",
                "evse_id='two'\nnative_evse_id=2\nconnector_id='two'",
            ),
    ] {
        fs::write(&path, configuration).unwrap();
        let mut child = fixture.start();
        child.expect_configuration_failure().await;
    }
    fs::write(path, original).unwrap();
}

#[tokio::test]
async fn set_optin_does_not_enable_clear_or_the_disabled_station() {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let configuration = fs::read_to_string(&path).unwrap().replace(
        "clear_charging_profile=true",
        "clear_charging_profile=false",
    );
    fs::write(path, configuration).unwrap();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let result = exchange(
        &client,
        &fixture,
        &mut station,
        command(
            "enabled-set",
            1,
            "SetChargingProfile",
            profile(1, 117, "0", "A"),
        ),
        "Accepted",
    )
    .await;
    assert_eq!(
        result["charging_profile_16"]["action"],
        "SetChargingProfile"
    );
    deny(
        &client,
        &fixture,
        &mut station,
        command("disabled-clear", 0, "ClearChargingProfile", json!({})),
        PRIVILEGED,
        422,
    )
    .await;
}
