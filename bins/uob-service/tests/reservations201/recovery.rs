use super::support::*;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn lost_native_response_survives_real_restart_without_mutator_replay() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let body = command("lost-reserve", 1);
    let submission = begin(&client, &fixture, body.clone());
    assert_eq!(
        receive(&mut socket).await,
        json!([2, "lost-reserve", "ReserveNow", native(1)])
    );
    socket.close(None).await.unwrap();
    let uncertain = completed(submission).await;
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain["reservation_201"]["status"].is_null());
    assert_private(&fixture, &uncertain);
    drop(process);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let recovered = result(&client, &fixture, "lost-reserve").await;
    assert_eq!(
        recovered["reservation_201"]["reconciliation"]["state"],
        "uncertain"
    );
    let mut next = connected(&fixture, &client).await;
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
        duplicate.json::<Value>().await.unwrap()["result"]["reservation_201"],
        recovered["reservation_201"]
    );
    no_call(&mut next).await;
    let cancel = begin(
        &client,
        &fixture,
        cancel("explicit-cancel", &json!(reservation_id(1))),
    );
    assert_eq!(receive(&mut next).await[2], "CancelReservation");
    send(
        &mut next,
        json!([3,"explicit-cancel",{"status":"Accepted"}]),
    )
    .await;
    assert_eq!(
        completed(cancel).await["reservation_201"]["status"],
        "Accepted"
    );
    state(&client, &fixture, "lost-reserve", "cancelled").await;
}

#[tokio::test]
async fn a_real_late_ack_records_its_status_without_reviving_a_consumed_reservation() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let submission = begin(&client, &fixture, command("late-after-start", 1));
    assert_eq!(receive(&mut socket).await[2], "ReserveNow");
    station_call(
        &mut socket,
        "reported-start",
        "TransactionEvent",
        transaction(
            "reported-start",
            0,
            Some((TOKEN, "ISO14443")),
            reservation_id(1),
        ),
    )
    .await;
    let uncertain = completed(submission).await;
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain["reservation_201"]["status"].is_null());
    assert_eq!(
        uncertain["reservation_201"]["reconciliation"]["state"],
        "consumed"
    );
    send(
        &mut socket,
        json!([3,"late-after-start",{"status":"Accepted","statusInfo":{"reasonCode":"Late"}}]),
    )
    .await;
    let accepted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = result(&client, &fixture, "late-after-start").await;
            if value["reservation_201"]["status"] == "Accepted" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(accepted["lifecycle"]["stage"], "protocol_response");
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    assert_eq!(
        accepted["reservation_201"]["reconciliation"]["state"],
        "consumed"
    );
    assert_private(&fixture, &accepted);
}

#[tokio::test]
async fn trusted_idle_expiry_runs_even_after_the_socket_disconnects_without_station_traffic() {
    let fixture = fixture(true);
    let expiry = json!(uob_contracts::UtcTimestamp::new(
        time::OffsetDateTime::now_utc() + time::Duration::seconds(4)
    ));
    let mut private = provisioning();
    private["reservations"][0]["request"]["expiryDateTime"] = expiry.clone();
    save(&fixture, &private);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let mut body = command("idle-expiry", 1);
    body["operation"]["parameters"]["payload"]["expiryDateTime"] = expiry;
    let submission = begin(&client, &fixture, body);
    assert_eq!(receive(&mut socket).await[2], "ReserveNow");
    send(&mut socket, json!([3,"idle-expiry",{"status":"Accepted"}])).await;
    assert_eq!(
        completed(submission).await["reservation_201"]["reconciliation"]["state"],
        "active"
    );
    socket.close(None).await.unwrap();
    let expired = state(&client, &fixture, "idle-expiry", "expired").await;
    assert_eq!(expired["reservation_201"]["status"], "Accepted");
    assert_private(&fixture, &expired);
}
