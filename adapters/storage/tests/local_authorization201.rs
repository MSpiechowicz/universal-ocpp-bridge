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
        Store::open(&self.0, 201).unwrap()
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
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("SendLocalList").unwrap(),
        payload_schema: PayloadSchemaId::new(SEND_LOCAL_LIST_REFERENCE_SCHEMA_201).unwrap(),
        payload: json!({"versionNumber":1,"updateType":"Full","updateReference":format!("list201:{}", "a".repeat(64))}),
    });
    value
}
fn accepted(command: &Command<Value>) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_LOCAL_AUTHORIZATION_201,
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
        local_authorization_201: Some(LocalAuthorizationResult201::SendLocalList {
            version_number: 1,
            update_type: LocalListUpdateType201::Full,
            status: SendLocalListStatus201::Accepted,
        }),
        local_authorization_16: None,
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
                invalid.local_authorization_201 =
                    Some(LocalAuthorizationResult201::GetLocalListVersion { version_number: 1 });
            }
            1 => {
                invalid.local_authorization_201 =
                    Some(LocalAuthorizationResult201::SendLocalList {
                        version_number: 2,
                        update_type: LocalListUpdateType201::Full,
                        status: SendLocalListStatus201::Accepted,
                    });
            }
            2 => {
                invalid.local_authorization_201 =
                    Some(LocalAuthorizationResult201::SendLocalList {
                        version_number: 1,
                        update_type: LocalListUpdateType201::Differential,
                        status: SendLocalListStatus201::Accepted,
                    });
            }
            3 => {
                invalid.local_authorization_201 =
                    Some(LocalAuthorizationResult201::SendLocalList {
                        version_number: 1,
                        update_type: LocalListUpdateType201::Full,
                        status: SendLocalListStatus201::Failed,
                    });
            }
            4 => invalid.schema_version = ContractVersion::V1_INITIAL,
            5 => invalid.resource.station_id = StationId::new("foreign").unwrap(),
            _ => invalid.correlation_id = Some(CorrelationId::new("foreign").unwrap()),
        }
        assert!(persist(&store, invalid).await.is_err());
    }
    let mut stale = evidence.clone();
    stale.local_authorization_201 = None;
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
    assert!(!encoded.contains("list201:"));
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
                    json!({"versionNumber":1,"updateType":"Full","localAuthorizationList":[]});
            }
            1 => operation.payload["versionNumber"] = json!(0),
            2 => operation.payload["versionNumber"] = json!(-1),
            3 => operation.protocol = ProtocolEdition::Ocpp16j,
            _ => {
                invalid.resource.native_protocol_reference =
                    Some(NativeProtocolReference::Ocpp201 {
                        evse_id: 1,
                        connector_id: None,
                    });
            }
        }
        assert!(admit(&store, invalid).await.is_err());
    }
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn mixed_native_and_legacy_evidence_rejects_the_entire_atomic_write() {
    let database = Database::new();
    let store = database.open();
    let native_command = command("native-evidence");
    admit(&store, native_command.clone()).await.unwrap();
    let winner = accepted(&native_command);
    persist(&store, winner.clone()).await.unwrap();
    for family in ["profile16", "schedule16", "local16"] {
        let mut mixed = winner.clone();
        match family {
            "profile16" => {
                mixed.charging_profile_16 = Some(ChargingProfileResult16::ClearChargingProfile {
                    request: ClearChargingProfileRequest16 {
                        id: Some(1),
                        connector_id: None,
                        charging_profile_purpose: None,
                        stack_level: None,
                    },
                    status: ClearChargingProfileStatus16::Accepted,
                });
            }
            "schedule16" => {
                mixed.composite_schedule_16 = Some(CompositeScheduleResult16 {
                    request: CompositeScheduleRequest16 {
                        connector_id: 0,
                        duration: 60,
                        charging_rate_unit: None,
                    },
                    status: CompositeScheduleStatus16::Accepted,
                    connector_id: Some(0),
                    schedule_start: Some(native_command.admitted_at),
                    charging_schedule: Some(CompositeSchedule16 {
                        duration: Some(60),
                        start_schedule: None,
                        charging_rate_unit: CompositeScheduleRateUnit16::W,
                        min_charging_rate: None,
                        charging_schedule_period: vec![CompositeSchedulePeriod16 {
                            start_period: 0,
                            limit: ExactDecimal::new(1, 0),
                            number_phases: Some(1),
                        }],
                    }),
                });
            }
            _ => {
                mixed.local_authorization_16 = Some(LocalAuthorizationResult16::SendLocalList {
                    list_version: 1,
                    update_type: LocalListUpdateType16::Full,
                    status: SendLocalListStatus16::Accepted,
                });
            }
        }
        let companion = command(&format!("rolled-back-{family}"));
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(companion.clone());
        write.command_result = Some(mixed);
        let error = store
            .write_atomic(write)
            .await
            .expect_err("mixed evidence rejected");
        assert_eq!(
            error.code(),
            uob_application::StorageErrorCode::IntegrityFailure
        );
        assert!(
            store
                .command_by_request_id(companion.request_id.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .command_result_by_request_id(native_command.request_id.clone())
                .await
                .unwrap(),
            Some(winner.clone()),
        );
        assert!(matches!(
            admit(&store, companion).await.unwrap().command,
            Some(CommandAdmissionOutcome::Admitted),
        ));
    }
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(
        reopened
            .command_result_by_request_id(native_command.request_id)
            .await
            .unwrap(),
        Some(winner),
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
