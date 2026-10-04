use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::{AtomicStoreWrite, CommandAdmissionOutcome, OperationalStore};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;

type Store = SqliteOperationalStore<Value, String, String, String>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-local-list-{}.db", uuid::Uuid::new_v4())))
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
fn command(id: &str) -> Command<Value> {
    let mut value: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    value.request_id = RequestId::new(id).unwrap();
    value.resource.resource = None;
    value.resource.native_protocol_reference = None;
    value.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("SendLocalList").unwrap(),
        payload_schema: PayloadSchemaId::new(SEND_LOCAL_LIST_REFERENCE_SCHEMA_16).unwrap(),
        payload: json!({"listVersion":-2,"updateType":"Full","updateReference":format!("list16:{}", "a".repeat(64))}),
    });
    value
}
fn accepted(command: &Command<Value>) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_LOCAL_AUTHORIZATION_16,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle: CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
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
        configuration_201: None,
        local_authorization_16: Some(LocalAuthorizationResult16::SendLocalList {
            list_version: -2,
            update_type: LocalListUpdateType16::Full,
            status: SendLocalListStatus16::Accepted,
        }),
    }
}
async fn admit(
    store: &Store,
    command: Command<Value>,
) -> Result<uob_application::AtomicWriteOutcome, uob_application::StorageError> {
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command);
    store.write_atomic(write).await
}
async fn persist(
    store: &Store,
    result: CommandResult,
) -> Result<uob_application::AtomicWriteOutcome, uob_application::StorageError> {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await
}

#[tokio::test]
async fn exact_native_evidence_survives_stale_writers_and_reopen_without_list_contents() {
    let database = Database::new();
    let store = database.open();
    let command = command("native-update");
    admit(&store, command.clone()).await.unwrap();
    let evidence = accepted(&command);
    persist(&store, evidence.clone()).await.unwrap();
    for mutation in 0..7 {
        let mut invalid = evidence.clone();
        match mutation {
            0 => {
                invalid.local_authorization_16 =
                    Some(LocalAuthorizationResult16::GetLocalListVersion { list_version: -2 });
            }
            1 => {
                invalid.local_authorization_16 = Some(LocalAuthorizationResult16::SendLocalList {
                    list_version: 2,
                    update_type: LocalListUpdateType16::Full,
                    status: SendLocalListStatus16::Accepted,
                });
            }
            2 => {
                invalid.local_authorization_16 = Some(LocalAuthorizationResult16::SendLocalList {
                    list_version: -2,
                    update_type: LocalListUpdateType16::Differential,
                    status: SendLocalListStatus16::Accepted,
                });
            }
            3 => {
                invalid.local_authorization_16 = Some(LocalAuthorizationResult16::SendLocalList {
                    list_version: -2,
                    update_type: LocalListUpdateType16::Full,
                    status: SendLocalListStatus16::Failed,
                });
            }
            4 => invalid.schema_version = ContractVersion::V1_INITIAL,
            5 => invalid.resource.station_id = StationId::new("foreign").unwrap(),
            _ => invalid.correlation_id = Some(CorrelationId::new("foreign").unwrap()),
        }
        assert!(persist(&store, invalid).await.is_err());
    }
    let mut stale = evidence.clone();
    stale.local_authorization_16 = None;
    stale.lifecycle = CommandLifecycle::TransmissionUncertain {
        detail: "response timeout".to_owned(),
    };
    persist(&store, stale).await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let store = database.open();
    assert!(
        matches!(admit(&store, command).await.unwrap().command, Some(CommandAdmissionOutcome::Duplicate { result: Some(result) }) if *result == evidence)
    );
    let encoded = serde_json::to_string(&evidence).unwrap();
    assert!(!encoded.contains("list16:"));
    assert!(!encoded.contains("localAuthorizationList"));
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn invalid_direct_native_list_admission_never_reaches_durable_storage() {
    let database = Database::new();
    let store = database.open();
    for mutation in 0..5 {
        let mut invalid = command(&format!("invalid-{mutation}"));
        let CommandOperation::Ocpp(operation) = &mut invalid.operation else {
            unreachable!()
        };
        match mutation {
            0 => {
                operation.payload =
                    json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[]});
            }
            1 => operation.payload["listVersion"] = json!(0),
            2 => operation.payload["listVersion"] = json!(-1),
            3 => operation.protocol = ProtocolEdition::Ocpp201,
            _ => {
                invalid.resource.native_protocol_reference =
                    Some(NativeProtocolReference::Ocpp16 { connector_id: 1 });
            }
        }
        assert!(admit(&store, invalid).await.is_err());
    }
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
