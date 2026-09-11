use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::{
    AuthorizationProvider, AuthorizationProviderDescriptor, AuthorizationProviderFuture,
    AuthorizationState, OperationalStore, PageLimit, RecoveryQuery, SensitiveAuthorizationToken,
};
use uob_protocol_adapter::{OcppErrorCode, v16};
use uob_provider_adapter::LocalAuthorizationProvider;

struct Delayed;
impl AuthorizationProvider for Delayed {
    fn descriptor(&self) -> AuthorizationProviderDescriptor {
        LocalAuthorizationProvider.descriptor()
    }
    fn resolve<'a>(
        &'a self,
        token: &'a SensitiveAuthorizationToken,
    ) -> AuthorizationProviderFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            LocalAuthorizationProvider.resolve(token).await
        })
    }
}

#[tokio::test]
async fn delayed_authorization_rechecks_policy_and_timeout_fails_closed() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    let before = state.clone();
    let mut services = services(&store, &auth);
    services.provider = &Delayed;
    let result = v16::transaction_call(START, &mut state, &services, context(&before, 1));
    let revoke = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        allow(&auth, &before, AuthorizationState::Revoked, 2, None).await;
    };
    let (result, ()) = tokio::join!(result, revoke);
    assert_eq!(result.unwrap()[2]["idTagInfo"]["status"], "Blocked");
    call(&store, &auth, &mut state, STOP, 2).await.unwrap();
    services.authorization_timeout = Duration::from_millis(5);
    let next = altered(
        START,
        "timestamp",
        json!("2026-09-01T01:30:00Z"),
        "new-start",
    );
    let ctx = context(&state, 3);
    assert_eq!(
        v16::transaction_call(&next, &mut state, &services, ctx)
            .await
            .unwrap()[2]["idTagInfo"]["status"],
        "Invalid"
    );
    assert_eq!(persisted(&store).await, state);
}

#[tokio::test]
async fn atomic_failure_has_no_success_or_partial_stop_and_retry_can_commit() {
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    call(&store, &auth, &mut state, START, 1).await.unwrap();
    let before = state.clone();
    // Deliberately collide with the already committed event ID and resource sequence.
    assert_eq!(
        call(&store, &auth, &mut state, STOP, 1)
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::InternalError
    );
    assert_eq!(state, before);
    assert_eq!(persisted(&store).await, before);
    assert_eq!(
        store
            .recover(RecoveryQuery {
                limit: PageLimit::new(100).unwrap()
            })
            .await
            .unwrap()
            .pending_deliveries
            .len(),
        1
    );
    call(&store, &auth, &mut state, STOP, 2).await.unwrap();
    let before = state.clone();
    store.shutdown(Duration::from_secs(2)).await.unwrap();
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
}

#[tokio::test]
async fn allocations_are_unique_across_workers_connections_and_restart() {
    let db = Database::new();
    let first = Arc::new(db.open());
    let second = Arc::new(db.open());
    let mut tasks = Vec::new();
    for i in 0..40 {
        let store = if i % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        tasks.push(tokio::spawn(
            async move { store.reserve_transaction_id().await },
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for task in tasks {
        // Bounded queue saturation is explicit; every successful reservation is still unique.
        if let Ok(id) = task.await.unwrap() {
            assert!(ids.insert(id));
        }
    }
    assert!(ids.len() >= 2);
    first.shutdown(Duration::from_secs(2)).await.unwrap();
    second.shutdown(Duration::from_secs(2)).await.unwrap();
    let next = db.open().reserve_transaction_id().await.unwrap();
    assert!(next > *ids.last().unwrap());
}

#[test]
fn decoder_bounds_nested_fields_and_never_debugs_presented_tags() {
    let decoded = v16::decode_call(START).unwrap();
    assert!(!format!("{decoded:?}").contains("INDEPENDENT-001"));
    for payload in [
        json!(null),
        json!([{"timestamp":"2026-09-01T01:00:00Z","sampledValue":[]}]),
        json!([{"timestamp":"2026-09-01T01:00:00Z","sampledValue":[{"value":"1","malicious":"<script>"}]}]),
        json!([{"timestamp":"2026-09-01T01:00:00Z","sampledValue":vec![json!({"value":"1"});257]}]),
    ] {
        assert!(v16::decode_call(&altered(STOP, "transactionData", payload, "invalid")).is_err());
    }
    assert!(v16::decode_call(&altered(STOP, "reason", json!("UnknownReason"), "invalid")).is_err());
    assert!(v16::decode_call(&vec![b' '; 256 * 1024 + 1]).is_err());
}
