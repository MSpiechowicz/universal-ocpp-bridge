use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
use uob_application::{
    OperationalStore, PageLimit, SnapshotQuery,
    data_transfer::{
        Capability, Error, Observation, OpaqueData, Provider, Registry, Reply, Status,
    },
    registration::RegistrationDecision,
};
use uob_contracts::{StationSnapshot, TypedValue, UtcTimestamp};
use uob_storage_adapter::SqliteOperationalStore;

pub type Store = SqliteOperationalStore<String, String, String, String>;
pub const TRANSFER: &[u8] =
    include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/1.6/data-transfer.json");
pub const BOOT: &[u8] =
    include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json");
pub const VENDOR: &str = "org.example.uob-test";

pub struct Database(PathBuf);
impl Database {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-transfer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn open(&self) -> Store {
        Store::open(self.0.join("state.db"), 8).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn now(second: u8) -> UtcTimestamp {
    serde_json::from_value(json!(format!("2026-09-16T12:00:{second:02}Z"))).unwrap()
}
pub fn snapshot() -> StationSnapshot {
    serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap()
}
pub async fn registered(store: &Store) -> StationSnapshot {
    let mut state = snapshot();
    uob_protocol_adapter::v16::registration_call(
        BOOT,
        store,
        &mut state,
        RegistrationDecision::Accepted,
        60,
        now(0),
    )
    .await
    .unwrap();
    state
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
pub fn value<'a>(state: &'a StationSnapshot, name: &str) -> Option<&'a TypedValue> {
    state
        .current_values
        .iter()
        .find(|p| p.point_id.as_str() == format!("ocpp16/data-transfer/{name}"))
        .and_then(|p| p.value.as_ref())
}
pub fn request(payload: Value) -> Vec<u8> {
    serde_json::to_vec(&Value::Array(vec![
        json!(2),
        json!("fixture-16-transfer"),
        json!("DataTransfer"),
        payload,
    ]))
    .unwrap()
}
pub fn registry(provider: Arc<dyn Provider>) -> Registry {
    Registry::new(vec![(
        Capability {
            vendor_id: VENDOR.to_owned(),
            message_id: Some("Probe".to_owned()),
        },
        provider,
    )])
    .unwrap()
}

// Test-only vendor agreement: a pure probe; no charger state mutation is implied by Accepted.
pub struct Probe;
impl Provider for Probe {
    fn handle<'a>(
        &'a self,
        _station: &'a StationSnapshot,
        request: &'a Observation,
    ) -> Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>> {
        Box::pin(async move {
            if request.data.as_ref().map(OpaqueData::expose) == Some("ping") {
                Ok(Reply {
                    status: Status::Accepted,
                    data: Some(OpaqueData::new("pong".to_owned())?),
                })
            } else {
                Ok(Reply {
                    status: Status::Rejected,
                    data: None,
                })
            }
        })
    }
}
pub struct DelayedProbe(pub tokio::sync::Notify);
impl Provider for DelayedProbe {
    fn handle<'a>(
        &'a self,
        station: &'a StationSnapshot,
        request: &'a Observation,
    ) -> Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>> {
        Box::pin(async move {
            self.0.notified().await;
            Probe.handle(station, request).await
        })
    }
}
pub async fn shutdown(store: &Store) {
    store.shutdown(Duration::from_secs(2)).await.unwrap();
}
