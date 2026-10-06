use super::support::*;
use serde_json::json;

#[tokio::test]
async fn inline_identity_scope_mismatch_unowned_reference_and_missing_privilege_never_reach_wire() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    for (id, credential, field, value) in [
        (
            "inline-token",
            PRIVILEGED,
            "idToken",
            json!({"idToken":TOKEN,"type":"ISO14443"}),
        ),
        (
            "inline-group",
            PRIVILEGED,
            "groupIdToken",
            json!({"idToken":GROUP,"type":"Central"}),
        ),
        ("wrong-evse", PRIVILEGED, "evseId", json!(2)),
        ("changed-id", PRIVILEGED, "id", json!(12)),
        ("changed-type", PRIVILEGED, "connectorType", json!("cType2")),
        (
            "wrong-reference",
            PRIVILEGED,
            "reservationReference",
            json!(reference(999)),
        ),
        ("no-privilege", CONTROL, "id", json!(reservation_id(1))),
    ] {
        let mut body = command(id, 1);
        body["operation"]["parameters"]["payload"][field] = value;
        rejected(&client, &fixture, credential, &body).await;
        no_call(&mut socket).await;
    }
    let mut legacy = command("legacy-schema", 1);
    legacy["operation"]["parameters"]["payload_schema"] =
        json!("urn:uob:ocpp16:ReserveNowReference:1");
    rejected(&client, &fixture, PRIVILEGED, &legacy).await;
    no_call(&mut socket).await;
    let mut child_cancel = cancel("child-cancel", &json!(-1));
    child_cancel["resource"] = command("unused", 1)["resource"].clone();
    rejected(&client, &fixture, PRIVILEGED, &child_cancel).await;
    no_call(&mut socket).await;
    for (field, value) in [
        ("idToken", json!({"idToken":TOKEN,"type":"ISO14443"})),
        ("customData", json!({"vendorId":"vendor"})),
    ] {
        let mut raw_cancel = cancel("raw-cancel", &json!(-1));
        raw_cancel["operation"]["parameters"]["payload"][field] = value;
        rejected(&client, &fixture, PRIVILEGED, &raw_cancel).await;
        no_call(&mut socket).await;
    }
}

#[tokio::test]
async fn unspecified_evse_needs_explicit_support_and_reservation_actions_default_off() {
    for explicit_feature in [true, false] {
        let fixture = if explicit_feature {
            fixture(false)
        } else {
            let fixture = fixture(false);
            let path = fixture.root.join("bridge.toml");
            let config = std::fs::read_to_string(&path)
                .unwrap()
                .replace("reserve_now=true\ncancel_reservation=true\n", "");
            std::fs::write(path, config).unwrap();
            fixture
        };
        let mut process = fixture.start();
        fixture.ready(&mut process).await;
        let client = client();
        let mut socket = connected(&fixture, &client).await;
        rejected(&client, &fixture, PRIVILEGED, &command("unspecified", 6)).await;
        no_call(&mut socket).await;
        if !explicit_feature {
            rejected(&client, &fixture, PRIVILEGED, &command("evse", 1)).await;
            rejected(&client, &fixture, PRIVILEGED, &cancel("cancel", &json!(1))).await;
            no_call(&mut socket).await;
        }
    }
}

#[tokio::test]
async fn edition_specific_reservation_options_fail_startup_when_misapplied() {
    for (needle, replacement) in [
        (
            "reserve_non_evse_specific_supported=false",
            "reserve_connector_zero_supported=true",
        ),
        ("reservation201_file=", "reservation16_file="),
        // ReserveNow without the owner-only provider file is never startable.
        ("reservation201_file=", "#reservation201_file="),
    ] {
        let fixture = fixture(false);
        let path = fixture.root.join("bridge.toml");
        let config = std::fs::read_to_string(&path)
            .unwrap()
            .replacen(needle, replacement, 1);
        std::fs::write(path, config).unwrap();
        let mut process = fixture.start();
        process.expect_configuration_failure().await;
    }
}
