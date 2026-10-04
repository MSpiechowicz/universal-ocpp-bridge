use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn lost_write_response_remains_uncertain_after_real_daemon_restart_without_replay() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let body = command("lost-list-update", "SendLocalList", 1);
    let submission = begin(&client, &fixture, body.clone());
    assert_eq!(
        receive(&mut socket).await,
        json!([2, "lost-list-update", "SendLocalList", native(1)])
    );
    socket.close(None).await.unwrap();
    let response = submission.await.unwrap();
    assert_eq!(response.status(), 202);
    let uncertain = response.json::<Value>().await.unwrap()["result"].clone();
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain["local_authorization_201"].is_null());
    assert_private(&fixture, &uncertain);
    drop(process);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    assert_eq!(
        result(&client, &fixture, "lost-list-update").await,
        uncertain
    );
    let mut next = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut next).await;
    fixture.connected(&client, "station-a").await;
    no_call(&mut next).await;
    let duplicate = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 202);
    assert_eq!(
        duplicate.json::<Value>().await.unwrap()["result"],
        uncertain
    );
    no_call(&mut next).await;
    let query = begin(
        &client,
        &fixture,
        command("query-after-lost", "GetLocalListVersion", 0),
    );
    assert_eq!(receive(&mut next).await[2], "GetLocalListVersion");
    send(&mut next, json!([3,"query-after-lost",{"versionNumber":1}])).await;
    assert_eq!(
        query.await.unwrap().json::<Value>().await.unwrap()["result"]["local_authorization_201"]["version_number"],
        1
    );
    assert_eq!(
        result(&client, &fixture, "lost-list-update").await,
        uncertain
    );
    let resync = begin(
        &client,
        &fixture,
        command("explicit-full-resync", "SendLocalList", 1),
    );
    assert_eq!(
        receive(&mut next).await,
        json!([2, "explicit-full-resync", "SendLocalList", native(1)])
    );
    send(
        &mut next,
        json!([3,"explicit-full-resync",{"status":"Accepted"}]),
    )
    .await;
    assert_eq!(resync.await.unwrap().status(), 202);
}

#[tokio::test]
async fn malformed_or_foreign_native_responses_never_fabricate_acknowledgements() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    for (id, action, payload) in [
        (
            "malformed-update",
            "SendLocalList",
            json!({"status":"Accepted","idTag":TOKEN}),
        ),
        (
            "malformed-cache",
            "ClearCache",
            json!({"status":"NotSupported"}),
        ),
        (
            "malformed-version",
            "GetLocalListVersion",
            json!({"versionNumber":"1"}),
        ),
        (
            "legacy-update-status",
            "SendLocalList",
            json!({"status":"NotSupported"}),
        ),
        (
            "negative-version",
            "GetLocalListVersion",
            json!({"versionNumber":-1}),
        ),
        (
            "overflow-version",
            "GetLocalListVersion",
            json!({"versionNumber":2_147_483_648_u64}),
        ),
    ] {
        let submission = begin(&client, &fixture, command(id, action, 1));
        receive(&mut socket).await;
        send(
            &mut socket,
            json!([3,"foreign-message-id",{"status":"Accepted"}]),
        )
        .await;
        send(&mut socket, json!([3, id, payload])).await;
        let response = submission.await.unwrap();
        assert_eq!(response.status(), 202);
        let value = response.json::<Value>().await.unwrap()["result"].clone();
        assert_eq!(value["lifecycle"]["stage"], "transmission_uncertain");
        assert!(value["local_authorization_201"].is_null());
        assert_private(&fixture, &value);
    }
}
