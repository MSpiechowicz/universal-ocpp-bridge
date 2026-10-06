use super::support::*;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn lost_native_response_survives_real_restart_without_mutator_replay() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let body = command("lost-reserve", 1);
    let submission = begin(&client, &fixture, body.clone());
    assert_eq!(
        receive(&mut socket).await,
        json!([2, "lost-reserve", "ReserveNow", native(1)])
    );
    socket.close(None).await.unwrap();
    let uncertain = completed(submission).await;
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain["reservation_16"]["status"].is_null());
    assert_private(&fixture, &uncertain);
    drop(process);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let recovered = result(&client, &fixture, "lost-reserve").await;
    assert_eq!(
        recovered["reservation_16"]["reconciliation"]["state"],
        "uncertain"
    );
    assert!(recovered["reservation_16"]["status"].is_null());
    let mut next = fixture.station("station-a").await;
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
        duplicate.json::<Value>().await.unwrap()["result"]["reservation_16"],
        recovered["reservation_16"]
    );
    no_call(&mut next).await;
    let cancel = begin(
        &client,
        &fixture,
        cancel("explicit-cancel", &native(1)["reservationId"]),
    );
    assert_eq!(receive(&mut next).await[2], "CancelReservation");
    send(
        &mut next,
        json!([3,"explicit-cancel",{"status":"Accepted"}]),
    )
    .await;
    assert_eq!(
        completed(cancel).await["reservation_16"]["status"],
        "Accepted"
    );
    assert_eq!(
        result(&client, &fixture, "lost-reserve").await["reservation_16"]["reconciliation"]["state"],
        "cancelled"
    );
}
#[tokio::test]
async fn a_real_late_ack_records_actual_status_without_resurrecting_an_already_reported_transaction()
 {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let submission = begin(&client, &fixture, command("late-after-start", 1));
    assert_eq!(receive(&mut socket).await[2], "ReserveNow");
    let reply = station_call(&mut socket, "reported-start", "StartTransaction", json!({"connectorId":1,"idTag":TOKEN,"meterStart":0,"timestamp":now(),"reservationId":native(1)["reservationId"]})).await;
    assert_eq!(reply["idTagInfo"]["status"], "Accepted");
    let uncertain = completed(submission).await;
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain["reservation_16"]["status"].is_null());
    assert_eq!(
        uncertain["reservation_16"]["reconciliation"]["state"],
        "consumed"
    );
    send(
        &mut socket,
        json!([3,"late-after-start",{"status":"Accepted"}]),
    )
    .await;
    let accepted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = result(&client, &fixture, "late-after-start").await;
            if value["reservation_16"]["status"] == "Accepted" {
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
        accepted["reservation_16"]["reconciliation"]["state"],
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
    let mut private = provisioning(false);
    private["reservations"][0]["request"]["expiryDate"] = expiry.clone();
    save(&fixture, &private);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let mut body = command("idle-expiry", 1);
    body["operation"]["parameters"]["payload"]["expiryDate"] = expiry;
    let submission = begin(&client, &fixture, body);
    assert_eq!(receive(&mut socket).await[2], "ReserveNow");
    send(&mut socket, json!([3,"idle-expiry",{"status":"Accepted"}])).await;
    assert_eq!(
        completed(submission).await["reservation_16"]["reconciliation"]["state"],
        "active"
    );
    socket.close(None).await.unwrap();
    let expired = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value = result(&client, &fixture, "idle-expiry").await;
            if value["reservation_16"]["reconciliation"]["state"] == "expired" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(expired["reservation_16"]["status"], "Accepted");
    assert_private(&fixture, &expired);
}
