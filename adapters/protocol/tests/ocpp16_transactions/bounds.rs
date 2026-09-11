use super::support::*;
use serde_json::json;
use uob_application::{CommandClock, transaction16::MAX_RETAINED_TRANSACTIONS};
use uob_contracts::{TransactionId, TransactionState, UtcTimestamp};
use uob_protocol_adapter::{OcppErrorCode, v16};
struct Later;
impl CommandClock for Later {
    fn now(&self) -> UtcTimestamp {
        time("2026-09-10T02:00:00Z")
    }
}

#[tokio::test]
async fn retention_rejects_expired_replay_even_after_clock_correction() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    call(&store, &auth, &mut state, START, 1).await.unwrap();
    call(&store, &auth, &mut state, STOP, 2).await.unwrap();
    let next = altered(
        START,
        "timestamp",
        json!("2026-09-10T01:30:00Z"),
        "new-start",
    );
    let mut services = services(&store, &auth);
    services.clock = &Later;
    let ctx = context(&state, 3);
    v16::transaction_call(&next, &mut state, &services, ctx)
        .await
        .unwrap();
    assert_eq!(state.transactions.len(), 1);
    assert_eq!(
        state.transactions[0]
            .ocpp16
            .as_ref()
            .unwrap()
            .transaction_id,
        2
    );
    let mut recovered = persisted(&store).await;
    assert_eq!(
        call(&store, &auth, &mut recovered, START, 4)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::ProtocolError
    );
    assert_eq!(
        call(&store, &auth, &mut recovered, STOP, 4)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::ProtocolError
    );
    assert_eq!(state, recovered);
}

#[tokio::test]
async fn full_history_refuses_new_starts_without_blocking_active_completion() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    call(&store, &auth, &mut state, START, 1).await.unwrap();
    let original = state.transactions[0].clone();
    for index in 1..MAX_RETAINED_TRANSACTIONS {
        let mut old = original.clone();
        old.transaction_id = TransactionId::new(format!("old-{index}")).unwrap();
        old.state = TransactionState::Ended;
        old.ended_at = Some(time("2026-09-01T01:00:00Z"));
        let evidence = old.ocpp16.as_mut().unwrap();
        evidence.transaction_id = i32::try_from(index + 100).unwrap();
        evidence.start_message_id = format!("old-message-{index}");
        evidence.start_fingerprint = format!("old-fingerprint-{index}");
        state.transactions.push(old);
    }
    call(&store, &auth, &mut state, STOP, 2).await.unwrap();
    let before = state.clone();
    let next = altered(
        START,
        "timestamp",
        json!("2026-09-01T01:30:00Z"),
        "new-start",
    );
    assert_eq!(
        call(&store, &auth, &mut state, &next, 3)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::InternalError
    );
    assert_eq!(state, before);
    assert_eq!(persisted(&store).await, before);
}

#[tokio::test]
async fn registration_and_storage_pressure_gate_start_without_losing_existing_work() {
    let db = Database::new();
    let store = Store::open_with_retention_policy(
        db.0.join("state.db"),
        8,
        uob_storage_adapter::SqliteRetentionPolicy {
            budget_bytes: 8192,
            active_session_reserve_bytes: 8191,
        },
    )
    .unwrap();
    let (mut state, auth) = setup(&store).await;
    let before = state.clone();
    assert_eq!(
        call(&store, &auth, &mut state, START, 1)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::InternalError
    );
    assert_eq!(state, before);
    assert_eq!(persisted(&store).await, before);
    state.current_values.clear();
    assert_eq!(
        call(&store, &auth, &mut state, START, 1)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::ProtocolError
    );
}
