#![cfg(unix)]
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "reservations201/ingress.rs"]
mod ingress;
#[path = "reservations201/observations.rs"]
mod observations;
#[path = "reservations201/recovery.rs"]
mod recovery;
#[allow(dead_code)]
#[path = "reservations201/support.rs"]
mod support;
use serde_json::{Value, json};
use support::*;

#[tokio::test]
async fn actual_daemon_preserves_every_native_status_and_signed_ids_without_private_evidence() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let schemas: Value = client
        .get(fixture.url("/api/v1/command-schemas?station_id=station-a"))
        .bearer_auth(PRIVILEGED)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let encoded = schemas.to_string();
    assert!(encoded.contains("urn:uob:ocpp201:ReserveNowReference:1"));
    assert!(encoded.contains("urn:OCPP:Cp:2:2020:3:CancelReservationRequest"));
    assert!(
        !encoded.contains("idToken"),
        "no identity field is discoverable"
    );
    for (index, status) in ["Accepted", "Faulted", "Occupied", "Rejected", "Unavailable"]
        .into_iter()
        .enumerate()
    {
        let id = format!("reserve-status-{index}");
        let value = create(&client, &fixture, &mut socket, &id, index + 1, status).await;
        assert_eq!(value["schema_version"]["revision"], 12);
        assert_eq!(
            value["reservation_201"]["reservation_id"],
            reservation_id(index + 1)
        );
        assert_eq!(value["reservation_201"]["evse_id"], 1);
        assert_eq!(value["lifecycle"]["accepted"], status == "Accepted");
        assert!(value["reservation_16"].is_null());
        assert_eq!(result(&client, &fixture, &id).await, value);
    }
    let unspecified = create(&client, &fixture, &mut socket, "unspecified", 6, "Accepted").await;
    assert!(unspecified["reservation_201"]["evse_id"].is_null());
    for (id, status) in [
        ("cancel-rejected", "Rejected"),
        ("cancel-accepted", "Accepted"),
    ] {
        let submission = begin(&client, &fixture, cancel(id, &json!(reservation_id(6))));
        let call = receive(&mut socket).await;
        assert_eq!(
            call,
            json!([2,id,"CancelReservation",{"reservationId":reservation_id(6)}])
        );
        send(
            &mut socket,
            json!([3,id,{"status":status,"statusInfo":{"reasonCode":"Private"}}]),
        )
        .await;
        let value = completed(submission).await;
        assert_eq!(value["reservation_201"]["status"], status);
        assert!(!value.to_string().contains("Private"));
        assert_private(&fixture, &value);
    }
    let cancelled = result(&client, &fixture, "unspecified").await;
    assert_eq!(cancelled["reservation_201"]["status"], "Accepted");
    assert_eq!(
        cancelled["reservation_201"]["reconciliation"]["state"],
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
async fn wrong_malformed_vendor_and_callerror_acks_remain_statusless_uncertainty() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    for (index, response) in [
        json!([3,"malformed",{"status":"MadeUp"}]),
        json!([3,"private-extra",{"status":"Accepted","idToken":{"idToken":TOKEN,"type":"ISO14443"}}]),
        json!([4,"callerror","InternalError",TOKEN,{"groupIdToken":GROUP}]),
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
        assert!(value["reservation_201"]["status"].is_null());
        assert_eq!(
            value["reservation_201"]["reconciliation"]["state"],
            "uncertain"
        );
        assert_private(&fixture, &value);
    }
}
