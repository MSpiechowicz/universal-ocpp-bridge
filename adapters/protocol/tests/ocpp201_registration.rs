mod endpoint_support;
#[path = "ocpp201_registration/wire.rs"]
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
use uob_protocol_adapter::{OcppErrorCode, v201::registration_call};
use uob_storage_adapter::SqliteOperationalStore;
type Store = SqliteOperationalStore<String, String, String, String>;
const BOOT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/boot-notification.json");
const HEARTBEAT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/heartbeat.json");
const STATUS: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/status-notification.json");

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
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
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
            json!([3,"fixture-201-boot-001",{"currentTime":now(second),"interval":10,"status":decision.as_str()}])
        );
        assert_eq!(persisted(&store).await, state);
        assert_eq!(
            text(&state.current_values, "ocpp201/registration/status"),
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
        protocol: ProtocolEdition::Ocpp201,
        connected_at: now(30),
        last_message_at: None,
    };
    assert_eq!(
        call(&store, &mut recovered, HEARTBEAT, 31).await,
        json!([3,"fixture-201-heartbeat-001",{"currentTime":now(31)}])
    );
    assert_eq!(persisted(&store).await, recovered);
    assert_eq!(recovered.transactions, original_transactions);
    assert!(
        matches!(recovered.connectivity, Connectivity::Connected { last_message_at: Some(t), .. } if t == now(31))
    );
}

#[tokio::test]
async fn native_statuses_are_bounded_and_scoped_to_exact_evse_connector() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    call(&store, &mut state, BOOT, 0).await;
    assert_eq!(
        text(&state.current_values, "ocpp201/registration/boot_reason"),
        Some("PowerUp")
    );
    let other_resources = state.resources[1..].to_vec();
    let transactions = state.transactions.clone();
    let mut count = None;
    for (second, (status, availability)) in [
        ("Available", AvailabilityState::Available),
        ("Occupied", AvailabilityState::Occupied),
        ("Reserved", AvailabilityState::Occupied),
        ("Unavailable", AvailabilityState::Unavailable),
        ("Faulted", AvailabilityState::Faulted),
    ]
    .into_iter()
    .enumerate()
    {
        let second = u8::try_from(second + 1).unwrap();
        let frame = json!([2,"status","StatusNotification",{"evseId":1,"connectorId":1,"connectorStatus":status,"timestamp":now(second)}]);
        assert_eq!(
            call(
                &store,
                &mut state,
                &serde_json::to_vec(&frame).unwrap(),
                second
            )
            .await,
            json!([3, "status", {}])
        );
        assert_eq!(state.resources[0].availability, availability);
        assert_eq!(
            text(
                &state.resources[0].current_values,
                "ocpp201/evse-1/connector-1/status/status"
            ),
            Some(status)
        );
        if let Some(count) = count {
            assert_eq!(state.resources[0].current_values.len(), count);
        }
        count = Some(state.resources[0].current_values.len());
        assert_eq!(state.resources[1..], other_resources);
        assert_eq!(state.transactions, transactions);
        assert_eq!(persisted(&store).await, state);
    }
    let before = state.resources.clone();
    call(&store, &mut state, STATUS, 20).await; // Device time 00 predates current status 05.
    assert_eq!(state.resources, before);
    let frame = json!([2,"other-evse","StatusNotification",{"evseId":2,"connectorId":1,"connectorStatus":"Available","timestamp":now(21)}]);
    call(&store, &mut state, &serde_json::to_vec(&frame).unwrap(), 22).await;
    assert_eq!(state.resources[0], before[0]);
    assert_eq!(
        state.resources[2].availability,
        AvailabilityState::Available
    );
    let point = state.resources[2]
        .current_values
        .iter()
        .find(|v| v.point_id.as_str() == "ocpp201/evse-2/connector-1/status/status")
        .unwrap();
    assert_eq!(point.source_time, Some(now(21)));
    assert_eq!(point.observed_at, now(22));
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    drop(store);
    assert_eq!(persisted(&db.open()).await, state);
}

#[tokio::test]
async fn invalid_requests_and_failed_commits_do_not_mutate_state() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    call(&store, &mut state, BOOT, 0).await;
    let valid: Value = serde_json::from_slice(STATUS).unwrap();
    let mut invalid = Vec::new();
    for (key, value) in [
        ("evseId", json!(0)),
        ("connectorId", json!(0)),
        ("evseId", json!(-1)),
        ("evseId", json!(99)),
        ("connectorId", json!(99)),
        ("connectorStatus", json!("Charging")),
        ("timestamp", json!("bad")),
        ("timestamp", Value::Null),
        ("errorCode", json!("NoError")),
        ("customData", json!({})),
    ] {
        let mut frame = valid.clone();
        frame[3][key] = value;
        invalid.push(frame);
    }
    let mut missing = valid.clone();
    missing[3].as_object_mut().unwrap().remove("timestamp");
    invalid.push(missing);
    invalid.extend([
        json!([2,"bad","Heartbeat",{"extra":true}]),
        json!([2,"bad","BootNotification",{"reason":"PowerUp","chargingStation":{"model":"x".repeat(21),"vendorName":"v"}}]),
        json!([2,"bad","BootNotification",{"reason":"Invented","chargingStation":{"model":"m","vendorName":"v"}}]),
        json!([2,"bad","BootNotification",{"reason":"PowerUp","chargingStation":{"model":"m","vendorName":"v","modem":{"imsi":"x".repeat(21)}}}]),
    ]);
    for frame in invalid {
        let before = state.clone();
        let error = registration_call(
            &serde_json::to_vec(&frame).unwrap(),
            &store,
            &mut state,
            RegistrationDecision::Accepted,
            60,
            now(1),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.code,
            OcppErrorCode::PropertyConstraintViolation,
            "{frame}"
        );
        assert_eq!(state, before);
        assert_eq!(persisted(&store).await, before);
    }
    let before = state.clone();
    assert!(
        registration_call(
            BOOT,
            &store,
            &mut state,
            RegistrationDecision::Accepted,
            0,
            now(1)
        )
        .await
        .is_err()
    );
    assert_eq!(state, before);
    store.shutdown(Duration::from_secs(2)).await.unwrap();
    for frame in [BOOT, HEARTBEAT, STATUS] {
        assert_eq!(
            registration_call(
                frame,
                &store,
                &mut state,
                RegistrationDecision::Accepted,
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
}

#[tokio::test]
async fn schema_boundaries_extensions_and_protocol_isolation() {
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    let frame = json!([2,"boot","BootNotification",{"reason":"Triggered","chargingStation":{"model":"é".repeat(20),"vendorName":"","customData":{"vendorId":"v","opaque":{"errorCode":"not-core"}}}}]);
    call(&store, &mut state, &serde_json::to_vec(&frame).unwrap(), 0).await;
    assert_eq!(
        text(&state.current_values, "ocpp201/registration/boot_reason"),
        Some("Triggered")
    );
    let heartbeat = json!([2,"heartbeat","Heartbeat",{"customData":{"vendorId":"é".repeat(255),"opaque":null}}]);
    call(
        &store,
        &mut state,
        &serde_json::to_vec(&heartbeat).unwrap(),
        1,
    )
    .await;
    for frame in [json!([2, "bad", "DataTransfer", {}]), json!([2, "bad"])] {
        assert!(
            registration_call(
                &serde_json::to_vec(&frame).unwrap(),
                &store,
                &mut state,
                RegistrationDecision::Accepted,
                60,
                now(2)
            )
            .await
            .is_err()
        );
    }
    for connectivity in [
        Connectivity::Disconnected,
        Connectivity::Connected {
            protocol: ProtocolEdition::Ocpp16j,
            connected_at: now(0),
            last_message_at: None,
        },
    ] {
        state.connectivity = connectivity;
        for frame in [BOOT, HEARTBEAT, STATUS] {
            assert_eq!(
                registration_call(
                    frame,
                    &store,
                    &mut state,
                    RegistrationDecision::Accepted,
                    60,
                    now(3)
                )
                .await
                .unwrap_err()
                .code,
                OcppErrorCode::ProtocolError
            );
        }
    }
}
