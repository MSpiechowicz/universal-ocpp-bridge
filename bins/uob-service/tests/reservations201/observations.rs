use super::support::*;
use serde_json::json;

async fn end(socket: &mut Socket, id: &str) {
    let reply = station_call(socket, &format!("{id}-end"), "TransactionEvent", json!({"eventType":"Ended","timestamp":now(),"triggerReason":"StopAuthorized","seqNo":1,"transactionInfo":{"transactionId":id,"stoppedReason":"Local"},"evse":{"id":1,"connectorId":1}})).await;
    assert_eq!(reply, json!({}));
}

#[tokio::test]
async fn native_status_updates_terminate_the_sent_owner_and_are_acknowledged_after_commit() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    create(&client, &fixture, &mut socket, "removed", 1, "Accepted").await;
    create(&client, &fixture, &mut socket, "expired", 6, "Accepted").await;
    for (id, status) in [(1, "Removed"), (6, "Expired")] {
        let reply = station_call(
            &mut socket,
            &format!("update-{id}"),
            "ReservationStatusUpdate",
            json!({"reservationId":reservation_id(id),"reservationUpdateStatus":status}),
        )
        .await;
        assert_eq!(reply, json!({}));
    }
    let removed = state(&client, &fixture, "removed", "removed").await;
    assert_eq!(removed["reservation_201"]["status"], "Accepted");
    assert!(removed["reservation_201"]["reconciliation"]["source_time"].is_null());
    state(&client, &fixture, "expired", "expired").await;
    assert_eq!(
        station_call(
            &mut socket,
            "unknown",
            "ReservationStatusUpdate",
            json!({"reservationId":12345,"reservationUpdateStatus":"Removed"}),
        )
        .await,
        json!({}),
        "an unknown station reservation is acknowledged without inventing state"
    );
    for (id, payload) in [
        (
            "vendor",
            json!({"reservationId":1,"reservationUpdateStatus":"Removed","customData":{"vendorId":"v"}}),
        ),
        (
            "invented",
            json!({"reservationId":1,"reservationUpdateStatus":"Faulted"}),
        ),
    ] {
        send(
            &mut socket,
            json!([2, id, "ReservationStatusUpdate", payload]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[0], 4, "{id} is a CALLERROR");
    }
    assert_private(&fixture, &removed);
}

#[tokio::test]
async fn transaction_reservation_id_consumes_only_with_matching_evse_and_identity_or_group() {
    let fixture = fixture(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    create(&client, &fixture, &mut socket, "direct", 1, "Accepted").await;
    create(&client, &fixture, &mut socket, "group", 2, "Accepted").await;
    create(&client, &fixture, &mut socket, "tokenless", 3, "Accepted").await;
    let reply = station_call(
        &mut socket,
        "tx-direct",
        "TransactionEvent",
        transaction(
            "tx-direct",
            0,
            Some((&TOKEN.to_lowercase(), "ISO14443")),
            reservation_id(1),
        ),
    )
    .await;
    assert_eq!(
        reply["idTokenInfo"]["status"], "Invalid",
        "a transaction report is not an authorization grant"
    );
    let consumed = state(&client, &fixture, "direct", "consumed").await;
    assert!(consumed["reservation_201"]["reconciliation"]["source_time"].is_string());
    end(&mut socket, "tx-direct").await;
    for (id, token) in [
        ("tx-stranger", (STRANGER, "ISO14443")),
        ("tx-wrong-type", (TOKEN, "Local")),
    ] {
        station_call(
            &mut socket,
            id,
            "TransactionEvent",
            transaction(id, 0, Some(token), reservation_id(2)),
        )
        .await;
        end(&mut socket, id).await;
    }
    assert_eq!(
        result(&client, &fixture, "group").await["reservation_201"]["reconciliation"]["state"],
        "active",
        "neither an unrelated token nor another token type ends the reservation"
    );
    station_call(
        &mut socket,
        "tx-sibling",
        "TransactionEvent",
        transaction("tx-sibling", 0, Some((SIBLING, "Local")), reservation_id(2)),
    )
    .await;
    state(&client, &fixture, "group", "consumed").await;
    end(&mut socket, "tx-sibling").await;
    station_call(
        &mut socket,
        "tx-tokenless",
        "TransactionEvent",
        transaction("tx-tokenless", 0, None, reservation_id(3)),
    )
    .await;
    let tokenless = state(&client, &fixture, "tokenless", "consumed").await;
    assert_private(&fixture, &tokenless);
}
