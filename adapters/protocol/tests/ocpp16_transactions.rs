#[path = "ocpp16_transactions/bounds.rs"]
mod bounds;
#[path = "ocpp16_transactions/edges.rs"]
mod edges;
mod endpoint_support;
#[path = "ocpp16_transactions/support.rs"]
mod support;
#[path = "ocpp16_transactions/wire.rs"]
mod wire;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use uob_application::{
    AuthorizationState, OperationalStore, PageLimit, RecoveryQuery, RetainedEventQuery,
};
use uob_contracts::{NativeProtocolReference, TransactionState};
use uob_protocol_adapter::OcppErrorCode;

#[tokio::test]
async fn lifecycle_commits_events_and_outbox_before_reply_and_replays_after_restart() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    let reply = call(&store, &auth, &mut state, START, 1).await.unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/ocpp-fixtures/corpus/wire/1.6/start-transaction-response.json"
    ))
    .unwrap();
    assert_eq!(reply, expected);
    assert_eq!(persisted(&store).await, state);
    assert_eq!(state.transactions[0].state, TransactionState::Pending);
    let before = state.clone();
    // Same payload under a new CALL ID still denotes the same transaction.
    let retry = altered(START, "meterStart", json!(0), "retry-start");
    assert_eq!(
        call(&store, &auth, &mut state, &retry, 2).await.unwrap()[2],
        reply[2]
    );
    assert_eq!(state, before);
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(auth);
    drop(store);
    let store = db.open();
    let auth = Auth::recover(Arc::new(store.clone()), PageLimit::new(100).unwrap())
        .await
        .unwrap();
    let mut state = persisted(&store).await;
    // Revocation cannot mutate a response whose transaction was already committed.
    allow(&auth, &state, AuthorizationState::Revoked, 2, None).await;
    assert_eq!(
        call(&store, &auth, &mut state, START, 2).await.unwrap(),
        reply
    );
    assert_eq!(
        call(&store, &auth, &mut state, STOP, 2).await.unwrap(),
        json!([3, "fixture-16-stop-001", {}])
    );
    assert_eq!(persisted(&store).await, state);
    let transaction = &state.transactions[0];
    assert_eq!(transaction.state, TransactionState::Ended);
    assert_eq!(transaction.ended_at, Some(time("2026-09-01T01:02:00Z")));
    let evidence = transaction.ocpp16.as_ref().unwrap();
    assert_eq!(evidence.meter_start, 0);
    assert_eq!(evidence.meter_stop, Some(1500));
    assert_eq!(evidence.stop_reason.as_deref(), Some("Remote"));
    assert_eq!(
        evidence.transaction_data[0]
            .measurement
            .as_ref()
            .unwrap()
            .protocol_reference,
        Some(NativeProtocolReference::Ocpp16 { connector_id: 1 })
    );
    assert!(
        evidence.transaction_data[0]
            .point_id
            .as_str()
            .starts_with("ocpp16/connector-1/")
    );
    let encoded = serde_json::to_string(&state).unwrap();
    assert!(!encoded.contains("INDEPENDENT-001"));
    let before = state.clone();
    assert!(call(&store, &auth, &mut state, STOP, 3).await.is_ok());
    assert!(call(&store, &auth, &mut state, START, 3).await.is_ok());
    assert_eq!(state, before);
    let events = store
        .read_retained_events(RetainedEventQuery {
            resource: state.resources[0].resource.clone(),
            after: None,
            limit: PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(events.events.len(), 2);
    assert_eq!(events.events[1].payload, state.transactions[0]);
    assert_eq!(
        store
            .recover(RecoveryQuery {
                limit: PageLimit::new(100).unwrap()
            })
            .await
            .unwrap()
            .pending_deliveries
            .len(),
        2
    );
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(store);
    let store = db.open();
    let mut recovered = persisted(&store).await;
    assert!(call(&store, &auth, &mut recovered, STOP, 3).await.is_ok());
    assert_eq!(recovered, before);
    assert_eq!(store.reserve_transaction_id().await.unwrap(), 2);
}

#[tokio::test]
async fn denied_start_retains_reported_transaction_and_stop_needs_no_permission() {
    for (status, expiry, expected) in [
        (AuthorizationState::Revoked, None, "Blocked"),
        (
            AuthorizationState::Active,
            Some(time("2026-09-01T01:00:00Z")),
            "Expired",
        ),
    ] {
        let db = Database::new();
        let store = db.open();
        let (mut state, auth) = setup(&store).await;
        allow(&auth, &state, status, 2, expiry).await;
        assert_eq!(
            call(&store, &auth, &mut state, START, 1).await.unwrap()[2]["idTagInfo"]["status"],
            expected
        );
        assert_eq!(state.transactions.len(), 1);
        let mut stop: Value = serde_json::from_slice(STOP).unwrap();
        stop[3].as_object_mut().unwrap().remove("idTag");
        assert!(
            call(
                &store,
                &auth,
                &mut state,
                &serde_json::to_vec(&stop).unwrap(),
                2
            )
            .await
            .is_ok()
        );
        assert_eq!(
            persisted(&store).await.transactions[0].state,
            TransactionState::Ended
        );
    }
}

#[tokio::test]
async fn conflicting_invalid_and_out_of_order_calls_preserve_committed_state() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    for frame in [
        altered(START, "connectorId", json!(0), "bad"),
        altered(START, "connectorId", json!(99), "bad"),
        altered(START, "unknown", json!(true), "bad"),
        altered(START, "idTag", json!("x".repeat(21)), "bad"),
        STOP.to_vec(),
    ] {
        let before = state.clone();
        assert!(call(&store, &auth, &mut state, &frame, 1).await.is_err());
        assert_eq!(state, before);
        assert_eq!(persisted(&store).await, before);
    }
    call(&store, &auth, &mut state, START, 1).await.unwrap();
    for frame in [
        altered(START, "meterStart", json!(1), "fixture-16-transaction-001"),
        altered(START, "timestamp", json!("2026-09-01T00:03:00Z"), "overlap"),
        altered(STOP, "timestamp", json!("2026-08-31T00:00:00Z"), "old-stop"),
        altered(STOP, "transactionId", json!(42), "unknown-stop"),
    ] {
        let before = state.clone();
        assert_eq!(
            call(&store, &auth, &mut state, &frame, 2)
                .await
                .unwrap_err()
                .code,
            OcppErrorCode::ProtocolError
        );
        assert_eq!(state, before);
    }
    call(&store, &auth, &mut state, STOP, 2).await.unwrap();
    let conflict = altered(STOP, "meterStop", json!(2000), "changed-stop");
    assert_eq!(
        call(&store, &auth, &mut state, &conflict, 3)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::ProtocolError
    );
    assert_eq!(persisted(&store).await, state);
}

#[tokio::test]
async fn exact_payload_cannot_reuse_another_actions_retained_call_id() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    call(&store, &auth, &mut state, START, 1).await.unwrap();
    call(&store, &auth, &mut state, STOP, 2).await.unwrap();
    let before = state.clone();
    for frame in [
        altered(START, "meterStart", json!(0), "fixture-16-stop-001"),
        altered(STOP, "meterStop", json!(1500), "fixture-16-transaction-001"),
    ] {
        assert_eq!(
            call(&store, &auth, &mut state, &frame, 3)
                .await
                .unwrap_err()
                .code,
            OcppErrorCode::ProtocolError
        );
        assert_eq!(state, before);
    }
}
