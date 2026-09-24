use time::{Duration, OffsetDateTime};
use uob_application::{
    TransactionApplyError, TransactionApplyOutcome, TransactionEventKind,
    TransactionEventObservation, apply_transaction_event,
};
use uob_contracts::{ProtocolEdition, StationSnapshot, TransactionState, UtcTimestamp};

fn station() -> StationSnapshot {
    serde_json::from_slice(include_bytes!(
        "../../contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .expect("station fixture")
}

fn at(day: i64) -> UtcTimestamp {
    UtcTimestamp::new(
        OffsetDateTime::from_unix_timestamp(1_788_220_800).expect("base date")
            + Duration::days(day),
    )
}

fn event(id: usize, event: TransactionEventKind, day: i64) -> TransactionEventObservation {
    TransactionEventObservation {
        remote_start_id: None,
        protocol: ProtocolEdition::Ocpp201,
        event,
        native_transaction_id: format!("station-transaction-{id}"),
        native_resource: station().resources[0]
            .resource
            .native_protocol_reference
            .expect("native EVSE"),
        sequence_number: u32::from(event == TransactionEventKind::Ended),
        trigger_reason: "CablePluggedIn".to_owned(),
        charging_state: None,
        stopped_reason: None,
        occurred_at: at(day),
        measurements: None,
        payload_fingerprint: format!("fingerprint-{id}-{event:?}"),
    }
}

fn recover(snapshot: &StationSnapshot) -> StationSnapshot {
    serde_json::from_slice(&serde_json::to_vec(snapshot).expect("encode"))
        .expect("recover persisted snapshot")
}

#[test]
fn more_than_201_sequential_transactions_remain_bounded_and_expired_starts_cannot_replay() {
    let mut snapshot = station();
    let first = event(0, TransactionEventKind::Started, 0);
    for id in 0..220 {
        let day = i64::try_from(id).unwrap();
        let start = event(id, TransactionEventKind::Started, day);
        let end = event(id, TransactionEventKind::Ended, day);
        assert_eq!(
            apply_transaction_event(&mut snapshot, &start, at(day)),
            Ok(TransactionApplyOutcome::Applied)
        );
        assert_eq!(
            apply_transaction_event(&mut snapshot, &end, at(day)),
            Ok(TransactionApplyOutcome::Applied)
        );
        assert!(snapshot.transactions.len() <= 128);
        snapshot = recover(&snapshot);
    }
    assert!(
        !snapshot.transactions.iter().any(|transaction| {
            transaction.transaction_id.as_str() == first.native_transaction_id
        })
    );
    let unchanged = snapshot.clone();
    assert_eq!(
        apply_transaction_event(&mut snapshot, &first, at(220)),
        Err(TransactionApplyError::ExpiredStart)
    );
    assert_eq!(snapshot, unchanged);
    let recent = event(219, TransactionEventKind::Ended, 219);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &recent, at(220)),
        Ok(TransactionApplyOutcome::Duplicate)
    );
    let fresh = event(220, TransactionEventKind::Started, 220);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &fresh, at(220)),
        Ok(TransactionApplyOutcome::Applied)
    );
}

#[test]
fn saturated_active_transactions_are_not_evicted_and_fail_without_partial_changes() {
    let mut snapshot = station();
    for id in 0..128 {
        let start = event(
            id,
            TransactionEventKind::Started,
            i64::try_from(id).unwrap(),
        );
        assert_eq!(
            apply_transaction_event(&mut snapshot, &start, at(i64::try_from(id).unwrap())),
            Ok(TransactionApplyOutcome::Applied)
        );
    }
    snapshot = recover(&snapshot);
    let unchanged = snapshot.clone();
    let extra = event(128, TransactionEventKind::Started, 129);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &extra, at(129)),
        Err(TransactionApplyError::Capacity)
    );
    assert_eq!(snapshot, unchanged);
    let end = event(0, TransactionEventKind::Ended, 0);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &end, at(129)),
        Ok(TransactionApplyOutcome::Applied)
    );
    assert_eq!(snapshot.transactions[0].state, TransactionState::Ended);
    assert_eq!(snapshot.transactions.len(), 128);
    snapshot = recover(&snapshot);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &extra, at(129)),
        Ok(TransactionApplyOutcome::Applied)
    );
    assert!(snapshot.transactions.len() <= 128);
    assert!(
        snapshot.transactions.iter().any(|transaction| {
            transaction.transaction_id.as_str() == extra.native_transaction_id
        })
    );
    assert_eq!(
        snapshot
            .transactions
            .iter()
            .filter(|transaction| transaction.state != TransactionState::Ended)
            .count(),
        128
    );
}

#[test]
fn recent_ended_transactions_keep_exact_replay_until_safe_age_out() {
    let mut snapshot = station();
    for id in 0..128 {
        apply_transaction_event(
            &mut snapshot,
            &event(id, TransactionEventKind::Started, 0),
            at(0),
        )
        .unwrap();
        apply_transaction_event(
            &mut snapshot,
            &event(id, TransactionEventKind::Ended, 0),
            at(0),
        )
        .unwrap();
    }
    snapshot = recover(&snapshot);
    let new_start = event(128, TransactionEventKind::Started, 1);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &new_start, at(1)),
        Err(TransactionApplyError::Capacity)
    );
    let last_end = event(127, TransactionEventKind::Ended, 0);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &last_end, at(1)),
        Ok(TransactionApplyOutcome::Duplicate)
    );
    let aged_start = event(128, TransactionEventKind::Started, 8);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &aged_start, at(8)),
        Ok(TransactionApplyOutcome::Applied)
    );
    snapshot = recover(&snapshot);
    assert!(snapshot.transactions.len() <= 128);
    assert_eq!(
        apply_transaction_event(
            &mut snapshot,
            &event(0, TransactionEventKind::Started, 0),
            at(8)
        ),
        Err(TransactionApplyError::ExpiredStart)
    );
}

#[test]
fn persisted_replay_floor_survives_clock_rollback_and_future_source_timestamps() {
    let mut snapshot = station();
    let stale_on_arrival = event(99, TransactionEventKind::Started, 0);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &stale_on_arrival, at(30)),
        Err(TransactionApplyError::ExpiredStart)
    );
    assert!(snapshot.transactions.is_empty());
    let early = event(0, TransactionEventKind::Started, 0);
    let early_end = event(0, TransactionEventKind::Ended, 0);
    apply_transaction_event(&mut snapshot, &early, at(0)).unwrap();
    apply_transaction_event(&mut snapshot, &early_end, at(0)).unwrap();
    let future = event(1, TransactionEventKind::Started, 300);
    apply_transaction_event(&mut snapshot, &future, at(30)).unwrap();
    let current = event(2, TransactionEventKind::Started, 30);
    apply_transaction_event(&mut snapshot, &current, at(30)).unwrap();
    snapshot = recover(&snapshot);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &early, at(1)),
        Err(TransactionApplyError::ExpiredStart)
    );
    assert_eq!(
        apply_transaction_event(&mut snapshot, &future, at(1)),
        Ok(TransactionApplyOutcome::Duplicate)
    );
    let old_unseen = event(3, TransactionEventKind::Started, 0);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &old_unseen, at(1)),
        Err(TransactionApplyError::ExpiredStart)
    );
    assert!(snapshot.transactions.iter().any(|transaction| {
        transaction.transaction_id.as_str() == future.native_transaction_id
    }));
}
