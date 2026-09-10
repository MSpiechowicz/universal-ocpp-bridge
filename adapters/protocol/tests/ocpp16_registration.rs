mod endpoint_support;
#[path = "ocpp16_registration/wire.rs"]
mod wire;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::{
    OperationalStore, PageLimit, SnapshotQuery, registration::RegistrationDecision,
};
use uob_contracts::{
    AvailabilityState, Connectivity, DataPointValue, ProtocolEdition, StationSnapshot, TypedValue,
    UtcTimestamp,
};
use uob_protocol_adapter::{OcppErrorCode, v16::registration_call};
use uob_storage_adapter::SqliteOperationalStore;
type Store = SqliteOperationalStore<String, String, String, String>;
const BOOT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json");
const HEARTBEAT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/heartbeat.json");
const STATUS: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/status-notification.json");

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-registration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> Store {
        Store::open(self.0.join("state.db"), 8).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn now(second: u8) -> UtcTimestamp {
    serde_json::from_value(json!(format!("2026-09-10T12:00:{second:02}Z"))).unwrap()
}
fn snapshot() -> StationSnapshot {
    serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap()
}
async fn persisted(store: &Store) -> StationSnapshot {
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
fn text<'a>(values: &'a [DataPointValue], id: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|v| v.point_id.as_str() == id)
        .and_then(|v| match &v.value {
            Some(TypedValue::Text(s)) => Some(s.as_str()),
            _ => None,
        })
}
async fn call(store: &Store, state: &mut StationSnapshot, bytes: &[u8], second: u8) -> Value {
    registration_call(
        bytes,
        store,
        state,
        RegistrationDecision::Accepted,
        60,
        now(second),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn boot_decisions_gate_heartbeat_and_survive_recovery() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    let original_transactions = state.transactions.clone();
    assert_eq!(
        registration_call(
            HEARTBEAT,
            &store,
            &mut state,
            RegistrationDecision::Accepted,
            60,
            now(0)
        )
        .await
        .unwrap_err()
        .code,
        OcppErrorCode::ProtocolError
    );
    for (second, decision) in [
        (0, RegistrationDecision::Pending),
        (10, RegistrationDecision::Rejected),
        (20, RegistrationDecision::Accepted),
    ] {
        let response = registration_call(BOOT, &store, &mut state, decision, 10, now(second))
            .await
            .unwrap();
        assert_eq!(
            response,
            json!([3,"fixture-16-boot-001",{"currentTime":now(second),"interval":10,"status":decision.as_str()}])
        );
        assert_eq!(persisted(&store).await, state);
        assert_eq!(
            text(&state.current_values, "ocpp16/registration/status"),
            Some(decision.as_str())
        );
        if decision != RegistrationDecision::Accepted {
            let before = state.clone();
            for frame in [HEARTBEAT, STATUS] {
                assert_eq!(
                    registration_call(
                        frame,
                        &store,
                        &mut state,
                        RegistrationDecision::Accepted,
                        10,
                        now(second + 1)
                    )
                    .await
                    .unwrap_err()
                    .code,
                    OcppErrorCode::ProtocolError
                );
                assert_eq!(state, before);
            }
        }
    }
    assert_eq!(state.transactions, original_transactions);
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(store);
    let store = db.open();
    let mut recovered = persisted(&store).await;
    recovered.connectivity = Connectivity::Connected {
        protocol: ProtocolEdition::Ocpp16j,
        connected_at: now(30),
        last_message_at: None,
    };
    assert_eq!(
        call(&store, &mut recovered, HEARTBEAT, 31).await,
        json!([3,"fixture-16-heartbeat-001",{"currentTime":now(31)}])
    );
    assert_eq!(persisted(&store).await, recovered);
    assert_eq!(recovered.transactions, original_transactions);
    assert!(
        matches!(recovered.connectivity, Connectivity::Connected { last_message_at: Some(t), .. } if t == now(31))
    );
}

#[tokio::test]
async fn status_preserves_errors_source_time_and_connector_topology() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    call(&store, &mut state, BOOT, 0).await;
    let resources = state
        .resources
        .iter()
        .map(|r| r.resource.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        call(&store, &mut state, STATUS, 5).await,
        json!([3, "fixture-16-status", {}])
    );
    let connector = &state.resources[0];
    assert_eq!(connector.availability, AvailabilityState::Faulted);
    assert_eq!(
        text(
            &connector.current_values,
            "ocpp16/connector-1/status/error_code"
        ),
        Some("GroundFailure")
    );
    assert_eq!(
        text(
            &connector.current_values,
            "ocpp16/connector-1/status/vendor_error_code"
        ),
        Some("E-GROUND")
    );
    let value = connector
        .current_values
        .iter()
        .find(|v| v.point_id.as_str() == "ocpp16/connector-1/status/status")
        .unwrap();
    assert_eq!(value.source_time, Some(now(0)));
    assert_eq!(value.observed_at, now(5));
    let mut report: Value = serde_json::from_slice(STATUS).unwrap();
    report[3] = json!({"connectorId":0,"status":"Unavailable","errorCode":"NoError"});
    call(&store, &mut state, &serde_json::to_vec(&report).unwrap(), 6).await;
    assert_eq!(
        text(&state.current_values, "ocpp16/connector-0/status/status"),
        Some("Unavailable")
    );
    assert_eq!(
        state
            .resources
            .iter()
            .map(|r| r.resource.clone())
            .collect::<Vec<_>>(),
        resources
    );
    assert_eq!(persisted(&store).await, state);
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(store);
    assert_eq!(persisted(&db.open()).await, state);
}

#[tokio::test]
async fn delayed_status_does_not_rewind_and_optional_errors_are_cleared() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    call(&store, &mut state, BOOT, 0).await;
    call(&store, &mut state, STATUS, 5).await;
    let mut report: Value = serde_json::from_slice(STATUS).unwrap();
    report[3] =
        json!({"connectorId":1,"status":"Charging","errorCode":"NoError","timestamp":now(10)});
    call(
        &store,
        &mut state,
        &serde_json::to_vec(&report).unwrap(),
        11,
    )
    .await;
    let current = state.resources[0].clone();
    call(&store, &mut state, STATUS, 12).await;
    assert_eq!(state.resources[0], current);
    assert_eq!(
        text(&current.current_values, "ocpp16/connector-1/status/status"),
        Some("Charging")
    );
    assert_eq!(
        text(
            &current.current_values,
            "ocpp16/connector-1/status/vendor_error_code"
        ),
        None
    );
    assert_eq!(persisted(&store).await, state);
    // Repeated reports replace the five slots, rather than growing history in snapshots.
    let count = state.resources[0].current_values.len();
    for second in 13..30 {
        call(
            &store,
            &mut state,
            &serde_json::to_vec(&report).unwrap(),
            second,
        )
        .await;
    }
    assert_eq!(state.resources[0].current_values.len(), count);
}

#[tokio::test]
async fn invalid_unsupported_and_storage_failure_never_report_success() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    call(&store, &mut state, BOOT, 0).await;
    for payload in [
        json!({"connectorId":99,"status":"Available","errorCode":"NoError"}),
        json!({"connectorId":0,"status":"Charging","errorCode":"NoError"}),
        json!({"connectorId":1,"status":"Invented","errorCode":"NoError"}),
        json!({"connectorId":1,"status":"Available","errorCode":"Invented"}),
        json!({"connectorId":-1,"status":"Available","errorCode":"NoError"}),
        json!({"connectorId":1,"status":"Available","errorCode":"NoError","timestamp":"not-time"}),
        json!({"connectorId":1,"status":"Available","errorCode":"NoError","info":"x".repeat(51)}),
        json!({"connectorId":1,"status":"Available","errorCode":"NoError","unknown":true}),
    ] {
        let before = state.clone();
        let frame = serde_json::to_vec(&json!([2, "bad", "StatusNotification", payload])).unwrap();
        assert_eq!(
            registration_call(
                &frame,
                &store,
                &mut state,
                RegistrationDecision::Accepted,
                60,
                now(1)
            )
            .await
            .unwrap_err()
            .code,
            OcppErrorCode::PropertyConstraintViolation
        );
        assert_eq!(state, before);
        assert_eq!(persisted(&store).await, before);
    }
    for (frame, code) in [
        (
            json!([2,"bad","Heartbeat",{"unexpected":true}]),
            OcppErrorCode::PropertyConstraintViolation,
        ),
        (
            json!([2,"bad","BootNotification",{"chargePointVendor":"x".repeat(21),"chargePointModel":"m"}]),
            OcppErrorCode::PropertyConstraintViolation,
        ),
        (
            json!([2, "bad", "DataTransfer", {}]),
            OcppErrorCode::NotImplemented,
        ),
        (json!([2, "bad"]), OcppErrorCode::FormationViolation),
    ] {
        assert_eq!(
            registration_call(
                &serde_json::to_vec(&frame).unwrap(),
                &store,
                &mut state,
                RegistrationDecision::Accepted,
                60,
                now(1)
            )
            .await
            .unwrap_err()
            .code,
            code
        );
    }
    let before = state.clone();
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    assert_eq!(
        registration_call(
            BOOT,
            &store,
            &mut state,
            RegistrationDecision::Rejected,
            60,
            now(2)
        )
        .await
        .unwrap_err()
        .code,
        OcppErrorCode::InternalError
    );
    assert_eq!(state, before);
}

#[tokio::test]
async fn all_native_statuses_and_schema_string_boundaries_are_preserved() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    // JSON Schema permits empty strings and counts Unicode characters rather than UTF-8 bytes.
    let boot = json!([2,"empty-boot","BootNotification",{"chargePointVendor":"","chargePointModel":"é".repeat(20),"chargeBoxSerialNumber":""}]);
    call(&store, &mut state, &serde_json::to_vec(&boot).unwrap(), 0).await;
    for (second, status) in [
        "Available",
        "Preparing",
        "Charging",
        "SuspendedEVSE",
        "SuspendedEV",
        "Finishing",
        "Reserved",
        "Unavailable",
        "Faulted",
    ]
    .into_iter()
    .enumerate()
    {
        let frame = json!([2,"status","StatusNotification",{"connectorId":1,"status":status,"errorCode":"NoError","info":"","vendorId":"é".repeat(255),"vendorErrorCode":""}]);
        call(
            &store,
            &mut state,
            &serde_json::to_vec(&frame).unwrap(),
            u8::try_from(second + 1).unwrap(),
        )
        .await;
        assert_eq!(
            text(
                &state.resources[0].current_values,
                "ocpp16/connector-1/status/status"
            ),
            Some(status)
        );
    }
    for bad in [
        json!([2,"bad","BootNotification",{"chargePointVendor":"v","chargePointModel":"m","chargeBoxSerialNumber":"x".repeat(26)}]),
        json!([2,"bad","StatusNotification",{"connectorId":1,"status":"Available","errorCode":"NoError","timestamp":null}]),
    ] {
        assert!(
            registration_call(
                &serde_json::to_vec(&bad).unwrap(),
                &store,
                &mut state,
                RegistrationDecision::Accepted,
                60,
                now(15)
            )
            .await
            .is_err()
        );
    }
    state.connectivity = Connectivity::Disconnected;
    assert!(
        registration_call(
            HEARTBEAT,
            &store,
            &mut state,
            RegistrationDecision::Accepted,
            60,
            now(16)
        )
        .await
        .is_err()
    );
}
