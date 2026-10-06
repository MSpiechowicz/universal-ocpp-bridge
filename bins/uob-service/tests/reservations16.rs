#![cfg(unix)]
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "reservations16/ingress.rs"]
mod ingress;
#[path = "reservations16/recovery.rs"]
mod recovery;
#[allow(dead_code)]
#[path = "reservations16/support.rs"]
mod support;
#[path = "reservations16/transactions.rs"]
mod transactions;
use serde_json::{Value, json};
use support::*;

#[tokio::test]
async fn actual_daemon_preserves_every_native_status_and_full_signed_ids_without_private_evidence()
{
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    for (index, status) in ["Accepted", "Faulted", "Occupied", "Rejected", "Unavailable"]
        .into_iter()
        .enumerate()
    {
        let id = format!("reserve-status-{index}");
        let value = create(&client, &fixture, &mut socket, &id, index + 1, status).await;
        assert_eq!(value["schema_version"]["revision"], 11);
        assert_eq!(
            value["reservation_16"]["reservation_id"],
            native(index + 1)["reservationId"]
        );
        assert_eq!(value["lifecycle"]["accepted"], status == "Accepted");
        assert_eq!(result(&client, &fixture, &id).await, value);
    }
    create(&client, &fixture, &mut socket, "any", 6, "Accepted").await;
    for (id, status) in [
        ("cancel-rejected", "Rejected"),
        ("cancel-accepted", "Accepted"),
    ] {
        let body = cancel(id, &native(6)["reservationId"]);
        let submission = begin(&client, &fixture, body);
        let call = receive(&mut socket).await;
        assert_eq!(
            call,
            json!([2,id,"CancelReservation",{"reservationId":native(6)["reservationId"]}])
        );
        send(&mut socket, json!([3,id,{"status":status}])).await;
        let value = completed(submission).await;
        assert_eq!(value["reservation_16"]["status"], status);
        assert_private(&fixture, &value);
    }
    let cancelled = result(&client, &fixture, "any").await;
    assert_eq!(cancelled["reservation_16"]["status"], "Accepted");
    assert_eq!(
        cancelled["reservation_16"]["reconciliation"]["state"],
        "cancelled"
    );
    let history = client
        .get(fixture.url("/api/v1/commands?station_id=station-a"))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap();
    assert_eq!(history.status(), 200);
    assert_private(&fixture, &history.json::<Value>().await.unwrap());
}
#[tokio::test]
async fn wrong_or_malformed_native_ack_and_callerror_remain_statusless_uncertainty() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    for (index, response) in [
        json!([3,"malformed",{"status":"MadeUp"}]),
        json!([3,"private-extra",{"status":"Accepted","idTag":TOKEN}]),
        json!([4,"callerror","InternalError",TOKEN,{"parentIdTag":PARENT}]),
    ]
    .into_iter()
    .enumerate()
    {
        let id = response[1].as_str().unwrap().to_owned();
        let submission = begin(&client, &fixture, command(&id, index + 1));
        assert_eq!(receive(&mut socket).await[2], "ReserveNow");
        send(&mut socket, response).await;
        let value = completed(submission).await;
        assert_eq!(value["lifecycle"]["stage"], "transmission_uncertain");
        assert!(value["reservation_16"]["status"].is_null());
        assert_eq!(
            value["reservation_16"]["reconciliation"]["state"],
            "uncertain"
        );
        assert_private(&fixture, &value);
    }
}
