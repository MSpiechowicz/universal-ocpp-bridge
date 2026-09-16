use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
use uob_application::{
    OperationalStore, PageLimit, SnapshotQuery,
    data_transfer201::{
        Capability, Error, Observation, OpaqueData, Provider, Registry, Reply, Status, StatusInfo,
    },
    registration::RegistrationDecision,
};
use uob_contracts::{StationId, StationSnapshot, TypedValue, UtcTimestamp};
use uob_storage_adapter::SqliteOperationalStore;

pub type Store = SqliteOperationalStore<String, String, String, String>;
pub const TRANSFER: &[u8] =
    include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/data-transfer.json");
pub const BOOT: &[u8] =
    include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/boot-notification.json");
pub const VENDOR: &str = "org.example.uob-test";
pub const FIXTURE_VENDOR: &str = "org.example.fixture";

pub struct Database(PathBuf);
impl Database {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-transfer201-{}", uuid::Uuid::new_v4()));
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
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap()
}
pub async fn registered(store: &Store, station_id: Option<&str>) -> StationSnapshot {
    let mut state = snapshot();
    if let Some(station_id) = station_id {
        state.station.station_id = StationId::new(station_id).unwrap();
    }
    uob_protocol_adapter::v201::registration_call(
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
            limit: PageLimit::new(10).unwrap(),
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
        .find(|point| point.point_id.as_str() == format!("ocpp201/data-transfer/{name}"))
        .and_then(|point| point.value.as_ref())
}
pub fn request(payload: Value) -> Vec<u8> {
    serde_json::to_vec(&Value::Array(vec![
        json!(2),
        json!("fixture-201-transfer"),
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
pub fn fixture_reply(status: Status) -> Reply {
    let accepted = status == Status::Accepted;
    Reply {
        status,
        data: Some(
            OpaqueData::new(if accepted {
                Value::Null
            } else {
                Value::Bool(false)
            })
            .unwrap(),
        ),
        custom_data: accepted.then(|| {
            OpaqueData::new(json!({"vendorId":FIXTURE_VENDOR,"opaque":{"reply":"accepted"}}))
                .unwrap()
        }),
        status_info: Some(StatusInfo {
            reason_code: if accepted { "Accepted" } else { "PolicyDenied" }.to_owned(),
            additional_info: Some(
                if accepted {
                    "secret://fixture/201-accepted"
                } else {
                    "secret://fixture/201-rejected"
                }
                .to_owned(),
            ),
            custom_data: accepted
                .then(|| {
                    OpaqueData::new(json!({"vendorId":FIXTURE_VENDOR,"opaque":[true,null]}))
                        .unwrap()
                })
                .or_else(|| {
                    Some(
                        OpaqueData::new(
                            json!({"vendorId":FIXTURE_VENDOR,"opaque":{"reason":"policy"}}),
                        )
                        .unwrap(),
                    )
                }),
        }),
    }
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
            let accepted = request
                .data
                .as_ref()
                .is_some_and(|data| data.expose().is_object() || data.expose().is_null());
            Ok(fixture_reply(if accepted {
                Status::Accepted
            } else {
                Status::Rejected
            }))
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
