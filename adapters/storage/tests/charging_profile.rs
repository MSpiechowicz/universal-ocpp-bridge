use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::{AtomicStoreWrite, OperationalStore};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<Value, String, String, String>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-profiles-{}.db", uuid::Uuid::new_v4())))
    }
    fn open(&self) -> Store {
        Store::open(&self.0, 16).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn at(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}
async fn admit(store: &Store, id: &str) -> Command<Value> {
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = RequestId::new(id).unwrap();
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("ClearChargingProfile").unwrap(),
        payload_schema: PayloadSchemaId::new("urn:OCPP:1.6:2019:12:ClearChargingProfileRequest")
            .unwrap(),
        payload: json!({"id":-117}),
    });
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    store.write_atomic(write).await.unwrap();
    command
}
fn result(command: &Command<Value>, lifecycle: CommandLifecycle) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at: command.admitted_at,
        observed_effects: vec![],
        configuration: None,
        configuration_observations: vec![],
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
    }
}
fn clear(id: i32, status: ClearChargingProfileStatus16) -> ChargingProfileResult16 {
    ChargingProfileResult16::ClearChargingProfile {
        request: ClearChargingProfileRequest16 {
            id: Some(id),
            connector_id: Some(999),
            charging_profile_purpose: Some(ChargingProfilePurpose16::TxProfile),
            stack_level: Some(3),
        },
        status,
    }
}
async fn write(store: &Store, result: CommandResult) {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}
async fn read(store: &Store, command: &Command<Value>) -> CommandResult {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn terminal_profile_identity_status_and_effects_survive_conflicting_writers_and_reopen() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "winning-profile").await;
    let mut winner = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
    );
    winner.schema_version = ContractVersion::V1_CHARGING_PROFILE_16;
    winner.recorded_at = at("2026-09-01T14:00:10Z");
    winner.charging_profile_16 = Some(clear(-117, ClearChargingProfileStatus16::Accepted));
    write(&store, winner.clone()).await;
    let other = database.open();
    let effect = ObservedCommandEffect {
        event_id: EventId::new("independent-observation").unwrap(),
        event_type: EventType::new("resource.status_changed").unwrap(),
        observed_at: at("2026-09-01T14:00:20Z"),
    };
    let mut stale = result(&command, CommandLifecycle::Dispatched);
    stale.observed_effects.push(effect.clone());
    write(&other, stale).await;
    winner.observed_effects.push(effect);
    assert_eq!(read(&store, &command).await, winner);
    let mut conflict = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: Some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                detail: None,
            }),
        },
    );
    conflict.schema_version = ContractVersion::V1_CHARGING_PROFILE_16;
    conflict.charging_profile_16 = Some(clear(118, ClearChargingProfileStatus16::Unknown));
    write(&other, conflict).await;
    write(
        &other,
        result(
            &command,
            CommandLifecycle::TransmissionUncertain {
                detail: "late disconnected writer".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(read(&store, &command).await, winner);
    let mut invalid = winner.clone();
    invalid.resource.station_id = StationId::new("foreign").unwrap();
    let mut update = AtomicStoreWrite::empty();
    update.command_result = Some(invalid);
    assert!(other.write_atomic(update).await.is_err());
    assert_eq!(read(&store, &command).await, winner);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    other.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(read(&reopened, &command).await, winner);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn late_native_profile_reply_cannot_resolve_terminal_uncertainty() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "uncertain-profile").await;
    let uncertain = result(
        &command,
        CommandLifecycle::TransmissionUncertain {
            detail: "socket lost after peer apply".to_owned(),
        },
    );
    write(&store, uncertain.clone()).await;
    let mut late = result(
        &command,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
    );
    late.schema_version = ContractVersion::V1_CHARGING_PROFILE_16;
    late.charging_profile_16 = Some(clear(-117, ClearChargingProfileStatus16::Accepted));
    write(&store, late).await;
    assert_eq!(read(&store, &command).await, uncertain);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(read(&reopened, &command).await, uncertain);
    let payload = serde_json::to_value(read(&reopened, &command).await).unwrap();
    assert!(payload.get("charging_profile_16").is_none());
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
