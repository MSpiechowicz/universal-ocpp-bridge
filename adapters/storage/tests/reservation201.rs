#[path = "reservation201/integrity.rs"]
mod integrity;
#[path = "reservation201/migration.rs"]
mod migration;
#[path = "reservation201/observations.rs"]
mod observations;
#[path = "reservation201/support.rs"]
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
    assert_eq!(
        state(&evidence(&store, &original).await),
        ReservationState201::Active
    );
    let replacement = reserve(&store, "replacement", i32::MIN, Some(candidate(2, 2)), 4)
        .await
        .unwrap();
    assert_eq!(
        reserve(&store, "overlap", 0, Some(candidate(0, 3)), 5)
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
            .is_none(),
        "a refused reservation mutation never admits its command"
    );
    dispatched(&store, &replacement, 5).await;
    response(&store, &replacement, "Occupied", 6).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState201::Active);
    assert_eq!(values[1].state, ReservationState201::Rejected);
    assert!(matches!(
        evidence(&store, &replacement).await,
        ReservationResult201::ReserveNow {
            evse_id: Some(2),
            status: Some(ReserveNowStatus201::Occupied),
            ..
        }
    ));
    let winner = reserve(&store, "winner", i32::MIN, Some(candidate(0, 2)), 7)
        .await
        .unwrap();
    dispatched(&store, &winner, 8).await;
    response(&store, &winner, "Accepted", 9).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState201::Superseded);
    assert_eq!(values[2].state, ReservationState201::Active);
    assert_eq!(values[2].candidate.as_ref().unwrap().evse_id, None);
    assert_eq!(
        state(&evidence(&store, &original).await),
        ReservationState201::Superseded,
        "public results follow the authoritative revision"
    );
    let rejected_cancel = reserve(&store, "cancel-rejected", i32::MIN, None, 10)
        .await
        .unwrap();
    dispatched(&store, &rejected_cancel, 11).await;
    response(&store, &rejected_cancel, "Rejected", 12).await;
    assert_eq!(records(&store).await[2].state, ReservationState201::Active);
    let cancel = reserve(&store, "cancel", i32::MIN, None, 13).await.unwrap();
    dispatched(&store, &cancel, 14).await;
    response(&store, &cancel, "Accepted", 15).await;
    let values = records(&store).await;
    assert_eq!(values[2].state, ReservationState201::Cancelled);
    assert_eq!(values[4].state, ReservationState201::Cancelled);
    assert!(
        store.reservations_16(station()).await.unwrap().is_empty(),
        "2.0.1 mutations never enter the 1.6 reservation owner"
    );
    for (id, value) in [(i32::MAX, 16), (-1, 20)] {
        let command = reserve(
            &store,
            &format!("max-{value}"),
            id,
            Some(candidate(3, 4)),
            value,
        )
        .await
        .unwrap();
        dispatched(&store, &command, value + 1).await;
        response(&store, &command, "Accepted", value + 2).await;
        assert!(matches!(
            evidence(&store, &command).await,
            ReservationResult201::ReserveNow { reservation_id, .. } if reservation_id == id
        ));
    }
}

#[tokio::test]
async fn lost_replies_stay_uncertain_and_restart_never_revives_an_unsent_mutator() {
    let database = Database::new();
    let store = database.open();
    let sent = reserve(&store, "sent", 7, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &sent, 2).await;
    persist(
        &store,
        result(
            &sent,
            CommandLifecycle::TransmissionUncertain {
                detail: "response timeout".to_owned(),
            },
            3,
        ),
    )
    .await;
    assert_eq!(
        records(&store).await[0].state,
        ReservationState201::Uncertain
    );
    let unsent = reserve(&store, "unsent", 8, Some(candidate(2, 2)), 4)
        .await
        .unwrap();
    drop(store);
    let store = database.open();
    store.recover_reservations_201(at(5)).await.unwrap();
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState201::Uncertain);
    assert!(values[0].unresolved, "a sent mutator keeps its uncertainty");
    assert_eq!(values[1].state, ReservationState201::Rejected);
    assert!(!values[1].unresolved, "an unsent mutator is never replayed");
    // A late valid acknowledgement resolves the sent owner without a resend.
    response(&store, &sent, "Accepted", 6).await;
    assert_eq!(records(&store).await[0].state, ReservationState201::Active);
    assert!(
        reserve(&store, "next", 9, Some(candidate(3, 3)), 7)
            .await
            .is_ok(),
        "recovery released the single-flight owner"
    );
    drop(unsent);
}

#[tokio::test]
async fn trusted_expiry_runs_without_station_traffic_and_a_late_ack_cannot_revive_it() {
    let database = Database::new();
    let store = database.open();
    let mut short = candidate(1, 1);
    short.expiry_date_time = at(50);
    let command = reserve(&store, "short", 1, Some(short), 1).await.unwrap();
    dispatched(&store, &command, 2).await;
    response(&store, &command, "Accepted", 3).await;
    store.expire_reservations_201(at(49)).await.unwrap();
    assert_eq!(records(&store).await[0].state, ReservationState201::Active);
    store.expire_reservations_201(at(50)).await.unwrap();
    assert_eq!(records(&store).await[0].state, ReservationState201::Expired);
    assert_eq!(
        state(&evidence(&store, &command).await),
        ReservationState201::Expired
    );
    observe(&store, update(1, ReservationUpdateStatus201::Removed), 51).await;
    assert_eq!(
        records(&store).await[0].state,
        ReservationState201::Expired,
        "a settled reservation is not rewritten by a later native update"
    );
    let mut late = candidate(2, 2);
    late.expiry_date_time = at(60);
    let late_command = reserve(&store, "late", 2, Some(late), 52).await.unwrap();
    dispatched(&store, &late_command, 53).await;
    response(&store, &late_command, "Accepted", 61).await;
    assert_eq!(
        records(&store).await[1].state,
        ReservationState201::Expired,
        "an acceptance recorded after expiry never becomes Active"
    );
}
