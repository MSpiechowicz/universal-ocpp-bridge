use super::support::*;

#[tokio::test]
async fn real_matching_start_before_ack_consumes_once_without_fabricating_native_acceptance() {
    let database = Database::new();
    let store = database.open();
    let command = reserve(&store, "start-before-ack", 0, Some(candidate(3, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &command, 2).await;
    for kind in [
        start(0, 4, 1, None, 3),
        start(1, 3, 1, None, 3),
        start(0, 3, 9, None, 3),
        start(0, 3, 7, Some(8), 3),
    ] {
        observe(&store, kind, 4).await;
    }
    assert_eq!(records(&store).await[0].state, ReservationState16::Pending);
    observe(&store, start(0, 3, 7, Some(9), 3), 5).await;
    assert_eq!(records(&store).await[0].state, ReservationState16::Consumed);
    assert!(matches!(
        evidence(&store, &command).await,
        ReservationResult16::ReserveNow { status: None, .. }
    ));
    // A physical observation does not finish the still-open native mutation socket.
    assert_eq!(
        reserve(&store, "still-in-flight", 1, None, 5)
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::Conflict
    );
    response(&store, &command, "Accepted", 6).await;
    persist(
        &store,
        result(
            &command,
            CommandLifecycle::TransmissionUncertain {
                detail: "late callback".to_owned(),
            },
            7,
        ),
    )
    .await;
    dispatched(&store, &command, 8).await;
    observe(&store, start(0, 3, 7, Some(9), 3), 9).await;
    let value = records(&store).await.remove(0);
    assert_eq!(value.state, ReservationState16::Consumed);
    assert_eq!(value.changed_at, at(5));
    assert_eq!(value.source_time, Some(at(3)));
    assert!(matches!(
        evidence(&store, &command).await,
        ReservationResult16::ReserveNow {
            status: Some(ReserveNowStatus16::Accepted),
            reconciliation: ReservationReconciliation16 {
                state: ReservationState16::Consumed,
                ..
            },
            ..
        }
    ));
    drop(store);
    let reopened = database.open();
    reopened.recover_reservations_16(at(10)).await.unwrap();
    assert_eq!(
        records(&reopened).await[0].state,
        ReservationState16::Consumed
    );
}

#[tokio::test]
async fn a_unique_live_tuple_uses_receipt_despite_a_slow_station_clock() {
    let database = Database::new();
    let store = database.open();
    let command = reserve(&store, "slow-clock", -1, Some(candidate(1, 1)), 10)
        .await
        .unwrap();
    dispatched(&store, &command, 11).await;
    observe(&store, start(-1, 1, 1, None, 9), 12).await;
    let record = records(&store).await.remove(0);
    assert_eq!(record.state, ReservationState16::Consumed);
    assert_eq!(record.source_time, Some(at(9)));
    assert_eq!(record.source_observed_at, Some(at(12)));
    assert!(
        matches!(evidence(&store, &command).await, ReservationResult16::ReserveNow { status: None, reconciliation: ReservationReconciliation16 { state: ReservationState16::Consumed, observed_at, source_time: Some(source), .. }, .. } if observed_at == at(12) && source == at(9))
    );
    response(&store, &command, "Accepted", 13).await;
    assert_eq!(records(&store).await[0].state, ReservationState16::Consumed);
}

#[tokio::test]
async fn reused_id_overlap_survives_ack_and_restart_without_widening_the_old_interval() {
    let database = Database::new();
    let store = database.open();
    let old = reserve(&store, "old", 7, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &old, 2).await;
    response(&store, &old, "Accepted", 3).await;
    let new = reserve(&store, "new", 7, Some(candidate(1, 1)), 10)
        .await
        .unwrap();
    dispatched(&store, &new, 11).await;
    response(&store, &new, "Accepted", 12).await;
    observe(&store, start(7, 1, 1, None, 11), 20).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState16::Superseded);
    assert_eq!(values[0].changed_at, at(12));
    assert_eq!(values[1].state, ReservationState16::Active);
    assert!(values[1].ambiguous);
    response(&store, &new, "Accepted", 21).await;
    assert!(matches!(
        evidence(&store, &new).await,
        ReservationResult16::ReserveNow {
            reconciliation: ReservationReconciliation16 {
                state: ReservationState16::Ambiguous,
                ..
            },
            ..
        }
    ));
    drop(store);
    let store = database.open();
    store.recover_reservations_16(at(22)).await.unwrap();
    assert!(records(&store).await[1].ambiguous);
    observe(&store, start(7, 1, 1, None, 4), 23).await;
    assert_eq!(
        records(&store).await[0].state,
        ReservationState16::Superseded
    );
    assert_eq!(records(&store).await[1].state, ReservationState16::Active);
    // Source 13 is after the old ownership cutoff, not after the late receipt 20.
    observe(&store, start(7, 1, 1, None, 13), 24).await;
    assert_eq!(
        records(&store).await[0].state,
        ReservationState16::Superseded
    );
    assert_eq!(records(&store).await[1].state, ReservationState16::Consumed);
    assert!(matches!(
        evidence(&store, &new).await,
        ReservationResult16::ReserveNow {
            reconciliation: ReservationReconciliation16 {
                state: ReservationState16::Consumed,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn a_distinct_connector_proves_a_late_historical_start_without_consuming_the_new_revision() {
    let database = Database::new();
    let store = database.open();
    let old = reserve(&store, "old-connector", 7, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &old, 2).await;
    response(&store, &old, "Accepted", 3).await;
    let new = reserve(&store, "new-connector", 7, Some(candidate(2, 1)), 10)
        .await
        .unwrap();
    dispatched(&store, &new, 11).await;
    response(&store, &new, "Accepted", 12).await;
    observe(&store, start(7, 1, 1, None, 4), 20).await;
    let values = records(&store).await;
    assert_eq!(values[0].state, ReservationState16::Consumed);
    assert_eq!(values[0].changed_at, at(12));
    assert_eq!(values[1].state, ReservationState16::Active);
    assert!(!values[1].ambiguous);
    assert!(
        matches!(evidence(&store, &old).await, ReservationResult16::ReserveNow { reconciliation: ReservationReconciliation16 { state: ReservationState16::Consumed, observed_at, source_time: Some(source), .. }, .. } if observed_at == at(20) && source == at(4))
    );
}

#[tokio::test]
async fn restart_clears_the_dead_socket_even_after_pre_ack_consumption() {
    let database = Database::new();
    let store = database.open();
    let command = reserve(&store, "consumed-open-socket", 0, Some(candidate(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &command, 2).await;
    observe(&store, start(0, 1, 1, None, 0), 3).await;
    drop(store);
    let store = database.open();
    store.recover_reservations_16(at(4)).await.unwrap();
    assert_eq!(records(&store).await[0].state, ReservationState16::Consumed);
    reserve(&store, "explicit-after-restart", 0, None, 5)
        .await
        .unwrap();
}
