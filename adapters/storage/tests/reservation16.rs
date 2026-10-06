#[path = "reservation16/integrity.rs"]
mod integrity;
#[path = "reservation16/migration.rs"]
mod migration;
#[path = "reservation16/observations.rs"]
mod observations;
#[path = "reservation16/retention.rs"]
mod retention;
#[path = "reservation16/support.rs"]
mod support;
use support::*;

#[tokio::test]
async fn signed_ids_single_flight_and_rejected_replacement_preserve_the_previous_owner() {
    let database = Database::new();
    let store = database.open();
    let original = reserve(&store, "original", i32::MIN, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &original, 2).await;
    response(&store, &original, "Accepted", 3).await;
    let replacement = reserve(&store, "replacement", i32::MIN, Some(candidate(2, 2)), 4)
        .await
        .unwrap();
    assert_eq!(records(&store).await[0].state, ReservationState16::Active);
    assert_eq!(
        reserve(&store, "overlap", 0, Some(candidate(1, 3)), 5)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Conflict
    );
    assert!(
        store
            .command_by_request_id(RequestId::new("overlap").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    dispatched(&store, &replacement, 5).await;
    response(&store, &replacement, "Occupied", 6).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState16::Active);
    assert_eq!(values[1].state, ReservationState16::Rejected);
    assert!(matches!(
        evidence(&store, &replacement).await,
        ReservationResult16::ReserveNow {
            status: Some(ReserveNowStatus16::Occupied),
            ..
        }
    ));
    let winner = reserve(&store, "winner", i32::MIN, Some(candidate(2, 2)), 7)
        .await
        .unwrap();
    dispatched(&store, &winner, 8).await;
    response(&store, &winner, "Accepted", 9).await;
    response(&store, &original, "Accepted", 10).await;
    assert_eq!(
        records(&store).await[0].state,
        ReservationState16::Superseded
    );
    assert_eq!(records(&store).await[2].state, ReservationState16::Active);
    let rejected_cancel = reserve(&store, "cancel-rejected", i32::MIN, None, 11)
        .await
        .unwrap();
    dispatched(&store, &rejected_cancel, 12).await;
    response(&store, &rejected_cancel, "Rejected", 13).await;
    assert_eq!(records(&store).await[2].state, ReservationState16::Active);
    let cancel = reserve(&store, "cancel", i32::MIN, None, 14).await.unwrap();
    dispatched(&store, &cancel, 15).await;
    response(&store, &cancel, "Accepted", 16).await;
    response(&store, &winner, "Accepted", 17).await;
    assert_eq!(
        records(&store).await[2].state,
        ReservationState16::Cancelled
    );
    assert_eq!(
        records(&store).await[4].state,
        ReservationState16::Cancelled
    );
}
#[tokio::test]
async fn trusted_expiry_and_actual_capacity_protect_against_stale_status_and_ack() {
    let database = Database::new();
    let store = database.open();
    let mut specific = candidate(1, 1);
    specific.expiry_date = at(10);
    let command = reserve(&store, "expiry", -1, Some(specific), 1)
        .await
        .unwrap();
    dispatched(&store, &command, 2).await;
    response(&store, &command, "Accepted", 3).await;
    observe(
        &store,
        ReservationObservationKind16::Status {
            connector_id: 1,
            state: ReservationState16::Faulted,
            source_time: Some(at(0)),
            any_eligible: true,
        },
        4,
    )
    .await;
    assert_eq!(records(&store).await[0].state, ReservationState16::Active);
    store.expire_reservations_16(at(10)).await.unwrap();
    response(&store, &command, "Accepted", 11).await;
    assert_eq!(records(&store).await[0].state, ReservationState16::Expired);
    let any = reserve(&store, "any", i32::MAX, Some(candidate(0, 2)), 12)
        .await
        .unwrap();
    dispatched(&store, &any, 13).await;
    response(&store, &any, "Accepted", 14).await;
    observe(
        &store,
        ReservationObservationKind16::Status {
            connector_id: 1,
            state: ReservationState16::Faulted,
            source_time: Some(at(15)),
            any_eligible: true,
        },
        15,
    )
    .await;
    assert_eq!(records(&store).await[1].state, ReservationState16::Active);
    observe(
        &store,
        ReservationObservationKind16::Status {
            connector_id: 2,
            state: ReservationState16::Unavailable,
            source_time: Some(at(16)),
            any_eligible: false,
        },
        16,
    )
    .await;
    response(&store, &any, "Accepted", 17).await;
    assert_eq!(
        records(&store).await[1].state,
        ReservationState16::Unavailable
    );
}
#[tokio::test]
async fn recovery_distinguishes_unsent_from_uncertain_mutators_and_explicit_capacity_fails_atomically()
 {
    let database = Database::new();
    let store = database.open();
    reserve(&store, "unsent", 1, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    store.recover_reservations_16(at(2)).await.unwrap();
    assert_eq!(records(&store).await[0].state, ReservationState16::Rejected);
    let sent = reserve(&store, "sent", 2, Some(candidate(1, 2)), 3)
        .await
        .unwrap();
    dispatched(&store, &sent, 4).await;
    store.recover_reservations_16(at(5)).await.unwrap();
    assert_eq!(
        records(&store).await[1].state,
        ReservationState16::Uncertain
    );
    for index in 2..MAX_RESERVATION_REVISIONS_16 {
        let command = reserve(
            &store,
            &format!("bounded-{index}"),
            i32::try_from(index).unwrap(),
            None,
            6,
        )
        .await
        .unwrap();
        dispatched(&store, &command, 6).await;
        response(&store, &command, "Rejected", 6).await;
    }
    assert_eq!(
        reserve(&store, "exhausted", 999, None, 7)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Conflict
    );
    assert!(
        store
            .command_by_request_id(RequestId::new("exhausted").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(records(&store).await.len(), MAX_RESERVATION_REVISIONS_16);
}
