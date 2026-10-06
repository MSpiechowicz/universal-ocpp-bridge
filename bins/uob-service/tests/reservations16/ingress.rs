use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn inline_identity_scope_mismatch_unowned_reference_and_missing_privilege_never_reach_wire() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    for (id, credential, field, value) in [
        ("inline-token", PRIVILEGED, "idTag", json!(TOKEN)),
        ("inline-parent", PRIVILEGED, "parentIdTag", json!(PARENT)),
        ("wrong-connector", PRIVILEGED, "connectorId", json!(2)),
        ("changed-id", PRIVILEGED, "reservationId", json!(12)),
        (
            "wrong-reference",
            PRIVILEGED,
            "reservationReference",
            json!(reference(999)),
        ),
        (
            "no-privilege",
            CONTROL,
            "reservationId",
            native(1)["reservationId"].clone(),
        ),
    ] {
        let mut body = command(id, 1);
        body["operation"]["parameters"]["payload"][field] = value;
        let response = client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(credential)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(
            !response.status().is_success(),
            "invalid reservation admitted: {id}"
        );
        let error: Value = response.json().await.unwrap();
        assert_private(&fixture, &error);
        no_call(&mut socket).await;
    }
    let mut child_cancel = cancel("child-cancel", &json!(-1));
    child_cancel["resource"] = command("unused", 1)["resource"].clone();
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&child_cancel)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    no_call(&mut socket).await;
    let mut raw_cancel = cancel("raw-cancel", &json!(-1));
    raw_cancel["operation"]["parameters"]["payload"]["idTag"] = json!(TOKEN);
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&raw_cancel)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    no_call(&mut socket).await;
}
#[tokio::test]
async fn connector_zero_is_not_offered_without_explicit_support_and_actions_default_off() {
    for explicit_feature in [true, false] {
        let fixture = if explicit_feature {
            fixture(false)
        } else {
            Fixture::new()
        };
        let mut process = fixture.start();
        fixture.ready(&mut process).await;
        let client = client();
        let mut socket = fixture.station("station-a").await;
        boot(&mut socket).await;
        fixture.connected(&client, "station-a").await;
        let body = command("zero-disabled", 6);
        let response = client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(PRIVILEGED)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success());
        no_call(&mut socket).await;
    }
}
