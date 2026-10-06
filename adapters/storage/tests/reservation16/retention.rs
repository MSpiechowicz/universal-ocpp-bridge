use super::support::*;
use serde_json::Value;

const HORIZON: i64 =
    OPERATIONAL_HISTORY_RETENTION_SECONDS + COMMAND_DEDUPLICATION_RETENTION_SECONDS;

fn lasting(connector_id: u32, key: u8) -> ReservationCandidate16 {
    ReservationCandidate16 {
        expiry_date: at(10 * HORIZON),
        ..candidate(connector_id, key)
    }
}

async fn settle(store: &Store, command: &Command<Value>, status: &str, now: i64) {
    dispatched(store, command, now).await;
    response(store, command, status, now + 1).await;
}

fn revisions(values: &[ReservationRecord16]) -> Vec<u64> {
    values.iter().map(|record| record.revision).collect()
}

#[tokio::test]
async fn settled_history_outlives_its_public_result_and_pruning_raises_the_start_floor() {
    let database = Database::new();
    let store = database.open();
    let live = reserve(&store, "live", 5, Some(lasting(2, 2)), 1)
        .await
        .unwrap();
    settle(&store, &live, "Accepted", 2).await;
    let old = reserve(&store, "old", 1, Some(candidate(1, 1)), 4)
        .await
        .unwrap();
    settle(&store, &old, "Accepted", 5).await;
    let cancel = reserve(&store, "cancel", 1, None, 7).await.unwrap();
    settle(&store, &cancel, "Accepted", 8).await;
    assert_eq!(records(&store).await[1].changed_at, at(9));

    // Outside the history window, a still-retained public result keeps its owner row.
    let probe = reserve(&store, "probe", 2, Some(lasting(3, 3)), HORIZON)
        .await
        .unwrap();
    settle(&store, &probe, "Rejected", HORIZON + 1).await;
    assert_eq!(revisions(&records(&store).await), [1, 2, 3, 4]);
    assert!(
        store
            .prune_command_deduplication(at(HORIZON + 3))
            .await
            .unwrap()
            >= 3
    );
    assert!(
        store
            .command_result_by_request_id(old.request_id.clone())
            .await
            .unwrap()
            .is_none()
    );

    let after = reserve(&store, "after", 3, Some(lasting(4, 4)), HORIZON + 10)
        .await
        .unwrap();
    let values = records(&store).await;
    assert_eq!(revisions(&values), [1, 4, 5]);
    assert_eq!(values[0].state, ReservationState16::Active);
    assert_eq!(values[1].history_floor, Some(at(9)));
    assert_eq!(values[2].history_floor, Some(at(9)));
    settle(&store, &after, "Rejected", HORIZON + 11).await;

    // Discarded history leaves a gap, so a start dated inside it cannot be attributed.
    observe(&store, start(5, 2, 2, None, 3), HORIZON + 13).await;
    assert_eq!(records(&store).await[0].state, ReservationState16::Active);
    drop(store);
    let reopened = database.open();
    reopened
        .recover_reservations_16(at(HORIZON + 14))
        .await
        .unwrap();
    assert_eq!(records(&reopened).await[2].history_floor, Some(at(9)));
    observe(&reopened, start(5, 2, 2, None, HORIZON + 15), HORIZON + 16).await;
    let value = records(&reopened).await.remove(0);
    assert_eq!(value.state, ReservationState16::Consumed);
    assert_eq!(value.source_time, Some(at(HORIZON + 15)));
}

#[tokio::test]
async fn recovery_prunes_settled_rows_but_keeps_unresolved_state_and_a_monotonic_head() {
    let database = Database::new();
    let store = database.open();
    let uncertain = reserve(&store, "uncertain", 1, Some(lasting(1, 1)), 1)
        .await
        .unwrap();
    dispatched(&store, &uncertain, 2).await;
    let lost = CommandLifecycle::TransmissionUncertain {
        detail: "socket closed".to_owned(),
    };
    persist(&store, result(&uncertain, lost, 3)).await;
    let settled = reserve(&store, "settled", 2, Some(candidate(2, 2)), 4)
        .await
        .unwrap();
    settle(&store, &settled, "Accepted", 5).await;
    let cancel = reserve(&store, "cancel", 2, None, 7).await.unwrap();
    settle(&store, &cancel, "Accepted", 8).await;
    assert_eq!(
        store
            .prune_command_deduplication(at(HORIZON))
            .await
            .unwrap(),
        2
    );
    drop(store);

    let reopened = database.open();
    reopened.recover_reservations_16(at(HORIZON)).await.unwrap();
    let values = records(&reopened).await;
    assert_eq!(revisions(&values), [1, 3]);
    assert!(values[0].unresolved);
    assert_eq!(values[1].state, ReservationState16::Cancelled);
    assert_eq!(values[1].history_floor, Some(at(9)));
    assert!(
        reopened
            .command_result_by_request_id(uncertain.request_id.clone())
            .await
            .unwrap()
            .is_some()
    );

    // Every row except the head may be settled and old; revisions are never reused.
    let next = reserve(&reopened, "next", 4, Some(lasting(3, 3)), HORIZON + 1)
        .await
        .unwrap();
    assert_eq!(revisions(&records(&reopened).await), [1, 3, 4]);
    settle(&reopened, &next, "Accepted", HORIZON + 2).await;
    assert_eq!(
        records(&reopened).await[2].state,
        ReservationState16::Active
    );
}
