use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use uob_application::{
    AuthorizationChange, AuthorizationProvider, AuthorizationState, CommandClock,
    LocalAuthorizationService, OperationalStore, PageLimit, SensitiveAuthorizationToken,
    SnapshotQuery, registration::RegistrationDecision, transaction16::TransactionContext,
};
use uob_contracts::{EventId, ServiceIdentity, StationSnapshot, TransactionSnapshot, UtcTimestamp};
use uob_protocol_adapter::v16::{self, TransactionServices};
use uob_provider_adapter::LocalAuthorizationProvider;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<String, TransactionSnapshot, TransactionSnapshot, String>;
pub type Auth = LocalAuthorizationService<String, TransactionSnapshot, TransactionSnapshot, String>;
pub const START: &[u8] =
    include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/1.6/start-transaction.json");
pub const STOP: &[u8] =
    include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/1.6/stop-transaction.json");
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-transactions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn open(&self) -> Store {
        Store::open(self.0.join("state.db"), 16).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn time(s: &str) -> UtcTimestamp {
    serde_json::from_value(json!(s)).unwrap()
}
pub struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        time("2026-09-01T02:00:00Z")
    }
}
pub fn context(state: &StationSnapshot, sequence: u64) -> TransactionContext {
    let identity: ServiceIdentity = serde_json::from_value(json!({
        "bridge_id": state.station.bridge_id,
        "runtime": {"environment":"demo", "release_id":"test", "release_digest":"sha256:test", "process_instance_id":"test-process"},
        "selected_target_id":"test-target"
    })).unwrap();
    TransactionContext {
        identity,
        event_id: EventId::new(format!("tx-event-{sequence}")).unwrap(),
        sequence,
        correlation_id: None,
        target: Some((
            uob_contracts::TargetInstanceId::new("test-target").unwrap(),
            1,
        )),
        delivery_deadline: time("2026-09-02T00:00:00Z"),
    }
}
pub async fn setup(store: &Store) -> (StationSnapshot, Auth) {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    snapshot.transactions.clear();
    v16::registration_call(
        include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json"),
        store,
        &mut snapshot,
        RegistrationDecision::Accepted,
        60,
        Clock.now(),
    )
    .await
    .unwrap();
    let auth = Auth::recover(Arc::new(store.clone()), PageLimit::new(100).unwrap())
        .await
        .unwrap();
    allow(&auth, &snapshot, AuthorizationState::Active, 1, None).await;
    (snapshot, auth)
}
pub async fn allow(
    auth: &Auth,
    state: &StationSnapshot,
    status: AuthorizationState,
    revision: u64,
    expires_at: Option<UtcTimestamp>,
) {
    let token = SensitiveAuthorizationToken::new("INDEPENDENT-001").unwrap();
    let reference = LocalAuthorizationProvider.resolve(&token).await.unwrap();
    auth.apply_change(AuthorizationChange {
        reference,
        resource: state.resources[0].resource.clone(),
        state: status,
        revision,
        changed_at: Clock.now(),
        expires_at,
    })
    .await
    .unwrap();
}
pub fn services<'a>(store: &'a Store, auth: &'a Auth) -> TransactionServices<'a, String, String> {
    TransactionServices {
        store,
        authorization: auth,
        provider: &LocalAuthorizationProvider,
        clock: &Clock,
        authorization_timeout: Duration::from_secs(1),
    }
}
pub async fn call(
    store: &Store,
    auth: &Auth,
    state: &mut StationSnapshot,
    frame: &[u8],
    sequence: u64,
) -> Result<Value, uob_protocol_adapter::OcppCallError> {
    let context = context(state, sequence);
    v16::transaction_call(frame, state, &services(store, auth), context).await
}
pub async fn persisted(store: &Store) -> StationSnapshot {
    store
        .read_snapshots(SnapshotQuery {
            after: None,
            limit: PageLimit::new(1).unwrap(),
        })
        .await
        .unwrap()
        .items
        .remove(0)
}
pub fn altered(frame: &[u8], field: &str, value: Value, id: &str) -> Vec<u8> {
    let mut frame: Value = serde_json::from_slice(frame).unwrap();
    frame[1] = json!(id);
    frame[3][field] = value;
    serde_json::to_vec(&frame).unwrap()
}
