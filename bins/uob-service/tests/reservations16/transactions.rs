use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn groups_are_native_parent_to_parent_evidence_not_policy_permission_or_parent_as_token() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    create(
        &client,
        &fixture,
        &mut socket,
        "group-reserve",
        1,
        "Accepted",
    )
    .await;
    for (id, token, expected) in [
        (
            "direct",
            TOKEN,
            json!({"status":"Accepted","parentIdTag":PARENT}),
        ),
        (
            "sibling",
            SIBLING,
            json!({"status":"Accepted","parentIdTag":"private-parent-113"}),
        ),
        ("ungranted-group", UNGRANTED, json!({"status":"Invalid"})),
        ("parent-as-token", PARENT, json!({"status":"Invalid"})),
        (
            "casefold-not-policy",
            "private-reserve-113",
            json!({"status":"Invalid"}),
        ),
    ] {
        let reply = station_call(&mut socket, id, "Authorize", json!({"idTag":token})).await;
        assert_eq!(reply, json!({"idTagInfo":expected}));
    }
    let start = station_call(&mut socket, "group-start", "StartTransaction", json!({"connectorId":1,"idTag":SIBLING,"meterStart":0,"timestamp":now(),"reservationId":native(1)["reservationId"]})).await;
    assert_eq!(start["idTagInfo"]["status"], "Accepted");
    let value = result(&client, &fixture, "group-reserve").await;
    assert_eq!(
        value["reservation_16"]["reconciliation"]["state"],
        "consumed"
    );
    assert_private(&fixture, &value);
    assert_private(&fixture, &fixture.snapshot(&client, "station-a").await);
}
#[tokio::test]
async fn current_policy_revocation_denies_but_still_persists_and_consumes_a_real_matching_start() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    create(
        &client,
        &fixture,
        &mut socket,
        "before-revoke",
        1,
        "Accepted",
    )
    .await;
    drop(process);
    save(&fixture, &provisioning(true));
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    no_call(&mut socket).await;
    assert_eq!(
        station_call(
            &mut socket,
            "revoked-authorize",
            "Authorize",
            json!({"idTag":TOKEN})
        )
        .await,
        json!({"idTagInfo":{"status":"Blocked"}})
    );
    let reply = station_call(&mut socket, "denied-real-start", "StartTransaction", json!({"connectorId":1,"idTag":TOKEN,"meterStart":17,"timestamp":now(),"reservationId":native(1)["reservationId"]})).await;
    assert_eq!(reply["idTagInfo"]["status"], "Blocked");
    assert!(reply["transactionId"].as_i64().unwrap() > 0);
    let snapshot = fixture.snapshot(&client, "station-a").await;
    let transaction = snapshot["transactions"]
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(transaction["state"], "pending");
    assert_eq!(transaction["ocpp16"]["authorization_status"], "Blocked");
    assert_eq!(
        transaction["ocpp16"]["reservation_id"],
        native(1)["reservationId"]
    );
    let consumed = result(&client, &fixture, "before-revoke").await;
    assert_eq!(consumed["reservation_16"]["status"], "Accepted");
    assert_eq!(
        consumed["reservation_16"]["reconciliation"]["state"],
        "consumed"
    );
    assert_private(&fixture, &snapshot);
    assert_private(&fixture, &consumed);
    let history = client
        .get(fixture.url("/api/v1/commands?station_id=station-a"))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_private(&fixture, &history);
}
