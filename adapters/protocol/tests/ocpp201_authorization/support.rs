use super::*;
use std::path::PathBuf;
use uob_application::{
    AuthorizationChange, AuthorizationReference, AuthorizationState, OperationalStore, PageLimit,
};
use uob_contracts::{BridgeId, StationId};
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;
pub type Authorization = LocalAuthorizationService<String, String, String, String>;
pub struct Database(PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-auth201-{}.sqlite", Uuid::new_v4())))
    }
    pub async fn recover(&self) -> Authorization {
        let store: Arc<dyn OperationalStore<String, String, String, String>> =
            Arc::new(SqliteOperationalStore::open(&self.0, 8).unwrap());
        LocalAuthorizationService::recover(store, PageLimit::new(16).unwrap())
            .await
            .unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
pub fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-test").unwrap(),
        station_id: StationId::new("station-a").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
pub fn timestamp(second: u8) -> UtcTimestamp {
    serde_json::from_str(&format!("\"2026-09-01T00:00:{second:02}Z\"")).unwrap()
}
pub struct Clock(pub AtomicU8);
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        timestamp(self.0.load(Ordering::SeqCst))
    }
}
pub async fn reference(frame: &[u8]) -> AuthorizationReference {
    let ChargerObservation::ChargingIdentity(identity) =
        v201::decode_call(frame).unwrap().observation
    else {
        panic!("identity")
    };
    let ChargingIdentityResolution::Resolved { reference, .. } = LocalChargingIdentityProvider
        .resolve(&identity)
        .await
        .unwrap()
    else {
        panic!("resolved")
    };
    reference
}
pub async fn allow(
    service: &Authorization,
    resource: ResourceRef,
    reference: AuthorizationReference,
    state: AuthorizationState,
    revision: u64,
    expires_at: Option<UtcTimestamp>,
) {
    service
        .apply_change(AuthorizationChange {
            reference,
            resource,
            state,
            revision,
            expires_at,
            changed_at: timestamp(0),
        })
        .await
        .unwrap();
}
pub async fn response(service: &Authorization, frame: &[u8], second: u8) -> Value {
    v201::authorize_call(
        frame,
        &resource(),
        service,
        &LocalChargingIdentityProvider,
        &Clock(AtomicU8::new(second)),
        Duration::from_secs(1),
    )
    .await
    .unwrap()
}
