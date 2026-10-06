use super::support::*;

async fn active(store: &Store, id: &str, reservation_id: i32, evse: u32, key: u8, now: i64) {
    let command = reserve(store, id, reservation_id, Some(candidate(evse, key)), now)
        .await
        .unwrap();
    dispatched(store, &command, now + 1).await;
    response(store, &command, "Accepted", now + 2).await;
}

#[tokio::test]
async fn native_expired_and_removed_updates_terminate_only_the_sent_live_owner() {
    let database = Database::new();
    let store = database.open();
    active(&store, "expired", 1, 1, 1, 1).await;
    active(&store, "removed", 2, 0, 2, 4).await;
    observe(&store, update(1, ReservationUpdateStatus201::Expired), 10).await;
    observe(&store, update(2, ReservationUpdateStatus201::Removed), 11).await;
    observe(&store, update(99, ReservationUpdateStatus201::Removed), 12).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState201::Expired);
    assert_eq!(values[0].source_observed_at, Some(at(10)));
    assert_eq!(
        values[0].source_time, None,
        "the native update has no timestamp"
    );
    assert_eq!(values[1].state, ReservationState201::Removed);
    assert_eq!(
        values.len(),
        2,
        "an unknown station reservationId creates nothing"
    );
    let command = reserve(&store, "cancel", 2, None, 13).await.unwrap();
    dispatched(&store, &command, 14).await;
    response(&store, &command, "Rejected", 15).await;
    assert_eq!(
        records(&store).await[1].state,
        ReservationState201::Removed,
        "H02.FR.01 rejection of an already removed reservation changes nothing"
    );
}

#[tokio::test]
async fn an_in_flight_same_id_replacement_makes_a_native_update_ambiguous() {
    let database = Database::new();
    let store = database.open();
    active(&store, "original", 5, 1, 1, 1).await;
    let replacement = reserve(&store, "replacement", 5, Some(candidate(2, 2)), 4)
        .await
        .unwrap();
    dispatched(&store, &replacement, 5).await;
    observe(&store, update(5, ReservationUpdateStatus201::Removed), 6).await;
    let values = records(&store).await;
    assert!(values.iter().all(|record| record.ambiguous));
    assert_eq!(values[0].state, ReservationState201::Active);
    assert_eq!(values[1].state, ReservationState201::Pending);
    assert_eq!(
        state(&evidence(&store, &replacement).await),
        ReservationState201::Ambiguous
    );
    response(&store, &replacement, "Accepted", 7).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState201::Superseded);
    assert_eq!(values[1].state, ReservationState201::Active);
}

#[tokio::test]
async fn transaction_reservation_id_consumes_once_with_evse_and_token_or_group_attribution() {
    let database = Database::new();
    let store = database.open();
    active(&store, "direct", 1, 1, 1, 1).await;
    active(&store, "group", 2, 0, 2, 4).await;
    active(&store, "tokenless", 3, 3, 3, 7).await;
    // Wrong EVSE or a token that is neither the reservation token nor its group never consumes.
    observe(&store, transaction(1, 2, Some(1), None, 20), 20).await;
    observe(&store, transaction(1, 1, Some(7), Some(8), 20), 20).await;
    assert_eq!(records(&store).await[0].state, ReservationState201::Active);
    observe(&store, transaction(1, 1, Some(1), None, 21), 21).await;
    observe(&store, transaction(2, 4, Some(5), Some(9), 22), 22).await;
    observe(&store, transaction(3, 3, None, None, 23), 23).await;
    let values = records(&store).await;
    for (index, source) in [(0, 21), (1, 22), (2, 23)] {
        assert_eq!(values[index].state, ReservationState201::Consumed);
        assert_eq!(values[index].source_time, Some(at(source)));
        assert!(!values[index].ambiguous);
    }
    observe(&store, transaction(1, 1, Some(1), None, 24), 30).await;
    let after = records(&store).await;
    assert_eq!(
        after[0].changed_at, values[0].changed_at,
        "consumed exactly once"
    );
    assert_eq!(after[0].source_time, Some(at(21)));
}

#[tokio::test]
async fn reused_ids_need_chronology_before_a_transaction_consumes_either_owner() {
    let database = Database::new();
    let store = database.open();
    active(&store, "first", 4, 1, 1, 1).await;
    let cancel = reserve(&store, "cancel", 4, None, 10).await.unwrap();
    dispatched(&store, &cancel, 11).await;
    response(&store, &cancel, "Accepted", 12).await;
    active(&store, "second", 4, 1, 1, 20).await;
    // A station start timestamped inside the first ownership interval belongs to neither the
    // live second owner nor, while a live candidate exists, the settled first one.
    observe(&store, transaction(4, 1, Some(1), None, 5), 30).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState201::Cancelled);
    assert_eq!(values[2].state, ReservationState201::Active);
    assert!(values[0].ambiguous && values[2].ambiguous);
    observe(&store, transaction(4, 1, Some(1), None, 31), 31).await;
    let values = records(&store).await;
    assert_eq!(values[2].state, ReservationState201::Consumed);
    assert!(!values[2].ambiguous);
}
