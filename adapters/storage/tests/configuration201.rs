use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::{AtomicStoreWrite, CommandAdmissionOutcome, OperationalStore};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
type Store = SqliteOperationalStore<Value, String, String, String>;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-config201-{}.db", uuid::Uuid::new_v4())))
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
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = RequestId::new(id).unwrap();
    command.resource.resource = None;
    command.resource.native_protocol_reference = None;
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("SetNetworkProfile").unwrap(),
        payload_schema: PayloadSchemaId::new(SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201).unwrap(),
        payload: json!({"configurationSlot":0,"profileReference":format!("cfg201:{}","a".repeat(64))}),
    });
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
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
        reservation_16: None,
        reservation_201: None,
        composite_schedule_201: None,
        charging_profiles_201: None,
    }
}
fn accepted(command: &Command<Value>) -> CommandResult {
    let mut value = result(
        command,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
    );
    value.schema_version = ContractVersion::V1_CONFIGURATION_201;
    value.configuration_201 = Some(ConfigurationResult201::SetNetworkProfile {
        configuration_slot: 0,
        status: SetNetworkProfileStatus201::Accepted,
        staged: true,
    });
    value
}
async fn admit(store: &Store, command: Command<Value>) -> uob_application::AtomicWriteOutcome {
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command);
    store.write_atomic(write).await.unwrap()
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
async fn typed_staging_dedup_and_original_command_integrity_survive_reopen() {
    let database = Database::new();
    let store = database.open();
    let command = command("staged");
    admit(&store, command.clone()).await;
    let evidence = accepted(&command);
    persist(&store, evidence.clone()).await.unwrap();
    assert!(
        matches!(admit(&store,command.clone()).await.command,Some(CommandAdmissionOutcome::Duplicate { result:Some(result) }) if *result==evidence)
    );
    for mutation in 0..4 {
        let mut bad = evidence.clone();
        match mutation {
            0 => bad.correlation_id = Some(CorrelationId::new("foreign").unwrap()),
            1 => {
                bad.configuration_201 = Some(ConfigurationResult201::SetNetworkProfile {
                    configuration_slot: 1,
                    status: SetNetworkProfileStatus201::Accepted,
                    staged: true,
                });
            }
            2 => {
                bad.configuration_201 = Some(ConfigurationResult201::SetNetworkProfile {
                    configuration_slot: 0,
                    status: SetNetworkProfileStatus201::Accepted,
                    staged: false,
                });
            }
            _ => {
                bad.lifecycle = CommandLifecycle::ProtocolResponse {
                    accepted: false,
                    error: None,
                }
            }
        }
        assert!(persist(&store, bad).await.is_err());
    }
    persist(&store, result(&command, CommandLifecycle::Dispatched))
        .await
        .unwrap();
    assert_eq!(
        store
            .command_result_by_request_id(command.request_id.clone())
            .await
            .unwrap()
            .unwrap(),
        evidence
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(
        reopened
            .command_result_by_request_id(command.request_id.clone())
            .await
            .unwrap()
            .unwrap(),
        evidence
    );
    assert!(
        reopened
            .recover(uob_application::RecoveryQuery {
                after_command: None,
                limit: uob_application::PageLimit::new(16).unwrap()
            })
            .await
            .unwrap()
            .active_commands
            .is_empty()
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
    for suffix in ["", "-wal"] {
        if let Ok(bytes) = std::fs::read(format!("{}{suffix}", database.0.display())) {
            assert!(
                !bytes
                    .windows(b"DO_NOT_PERSIST".len())
                    .any(|window| window == b"DO_NOT_PERSIST")
            );
            assert!(
                !bytes
                    .windows(b"connectionData".len())
                    .any(|window| window == b"connectionData")
            );
        }
    }
}

#[tokio::test]
async fn raw_values_profiles_and_unicode_duplicate_envelopes_never_reach_storage() {
    let database = Database::new();
    let store = database.open();
    let mut raw = command("raw-profile");
    let CommandOperation::Ocpp(operation) = &mut raw.operation else {
        panic!("operation");
    };
    operation.payload =
        json!({"configurationSlot":0,"connectionData":{"password":"DO_NOT_PERSIST"}});
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(raw);
    assert!(store.write_atomic(write).await.is_err());
    let mut duplicate = command("duplicate");
    let CommandOperation::Ocpp(operation) = &mut duplicate.operation else {
        panic!("operation");
    };
    operation.action = ProtocolActionName::new("SetVariables").unwrap();
    operation.payload_schema = PayloadSchemaId::new(SET_VARIABLES_REFERENCE_SCHEMA_201).unwrap();
    operation.payload = json!({"setVariableData":[
        {"component":{"name":"Straße"},"variable":{"name":"Value"},"valueReference":format!("cfg201:{}","a".repeat(64))},
        {"component":{"name":"STRASSE"},"variable":{"name":"VALUE"},"attributeType":"Actual","valueReference":format!("cfg201:{}","b".repeat(64))}
    ]});
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(duplicate);
    assert!(store.write_atomic(write).await.is_err());
    let mut raw = command("raw-variable");
    let CommandOperation::Ocpp(operation) = &mut raw.operation else {
        panic!("operation");
    };
    operation.action = ProtocolActionName::new("SetVariables").unwrap();
    operation.payload_schema = PayloadSchemaId::new(SET_VARIABLES_REFERENCE_SCHEMA_201).unwrap();
    operation.payload = json!({"setVariableData":[{"component":{"name":"Vendor"},"variable":{"name":"Secret"},"attributeValue":"DO_NOT_PERSIST"}]});
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(raw);
    assert!(store.write_atomic(write).await.is_err());
    assert!(
        store
            .command_by_request_id(RequestId::new("raw-profile").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let bytes = std::fs::read(&database.0).unwrap();
    assert!(
        !bytes
            .windows(b"DO_NOT_PERSIST".len())
            .any(|window| window == b"DO_NOT_PERSIST")
    );
}

#[tokio::test]
async fn interrupted_write_recovery_remains_uncertain_and_dedup_cannot_replay() {
    let database = Database::new();
    let store = database.open();
    let command = command("interrupted");
    admit(&store, command.clone()).await;
    let uncertain = result(
        &command,
        CommandLifecycle::TransmissionUncertain {
            detail: "disconnected after transmission".into(),
        },
    );
    persist(&store, uncertain.clone()).await.unwrap();
    persist(&store, accepted(&command)).await.unwrap();
    assert_eq!(
        store
            .command_result_by_request_id(command.request_id.clone())
            .await
            .unwrap()
            .unwrap(),
        uncertain
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    let recovery = reopened
        .recover(uob_application::RecoveryQuery {
            after_command: None,
            limit: uob_application::PageLimit::new(16).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(recovery.active_commands, vec![command.clone()]);
    assert_eq!(recovery.command_results, vec![uncertain.clone()]);
    assert!(
        matches!(admit(&reopened,command).await.command,Some(CommandAdmissionOutcome::Duplicate { result:Some(result) }) if *result==uncertain)
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn tampered_result_identity_is_rejected_on_read_and_recovery() {
    let database = Database::new();
    let store = database.open();
    let command = command("tampered");
    admit(&store, command.clone()).await;
    persist(&store, accepted(&command)).await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection.execute(
        "UPDATE command_results SET payload = json_set(payload, '$.configuration_201.configuration_slot', 1) WHERE request_id = ?1",
        [command.request_id.as_str()],
    ).unwrap();
    connection
        .execute(
            "UPDATE commands SET unresolved = 1 WHERE request_id = ?1",
            [command.request_id.as_str()],
        )
        .unwrap();
    drop(connection);
    let reopened = database.open();
    assert!(
        reopened
            .command_result_by_request_id(command.request_id.clone())
            .await
            .is_err()
    );
    assert!(
        reopened
            .recover(uob_application::RecoveryQuery {
                after_command: None,
                limit: uob_application::PageLimit::new(16).unwrap(),
            })
            .await
            .is_err()
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
