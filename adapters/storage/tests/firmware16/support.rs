use serde_json::{Value, json};
use std::path::PathBuf;
pub use uob_application::*;
pub use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, String, String, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-firmware16-{}.db", uuid::Uuid::new_v4())))
    }
    pub fn open(&self) -> Store {
        Store::open(&self.0, 32).unwrap()
    }
    /// Independent read of the release-drain inventory that the job owns.
    pub fn release_jobs(&self) -> Vec<(String, String)> {
        let connection = rusqlite::Connection::open(&self.0).unwrap();
        let mut statement = connection
            .prepare("SELECT id, kind FROM release_jobs ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
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
pub fn at(second: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second))
}
pub fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
pub fn command(id: &str, variant: FirmwareVariant16, now: i64) -> Command<Value> {
    let mut value: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    value.request_id = RequestId::new(id).unwrap();
    value.correlation_id = Some(CorrelationId::new(id).unwrap());
    value.resource = station();
    value.admitted_at = at(now);
    value.expires_at = at(now + 1000);
    let (action, schema, payload) = match variant {
        FirmwareVariant16::Legacy => (
            "UpdateFirmware",
            UPDATE_FIRMWARE_REFERENCE_SCHEMA_16,
            json!({"artifactReference":"image-1.bin","retrieveDate":at(now),"retries":2}),
        ),
        FirmwareVariant16::Signed { request_id } => (
            "SignedUpdateFirmware",
            SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16,
            json!({"requestId":request_id,"artifactReference":"image-1.bin","retrieveDateTime":at(now)}),
        ),
    };
    value.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(schema).unwrap(),
        payload,
    });
    value
}
pub fn result(command: &Command<Value>, lifecycle: CommandLifecycle, now: i64) -> CommandResult {
    let old: Value = serde_json::from_str(include_str!(
        "../../../../crates/contracts/tests/fixtures/command-results-v1.json"
    ))
    .unwrap();
    let mut value: CommandResult = serde_json::from_value(old[0].clone()).unwrap();
    value.resource.clone_from(&command.resource);
    value.correlation_id.clone_from(&command.correlation_id);
    value.return_route = command.return_route();
    value.lifecycle = lifecycle;
    value.recorded_at = at(now);
    value
}
pub async fn admit(
    store: &Store,
    id: &str,
    variant: FirmwareVariant16,
    now: i64,
) -> Result<Command<Value>, StorageError> {
    let command = command(id, variant, now);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted, now));
    write.firmware_16 = Some(Box::new(FirmwareJobMutation16 {
        station: station(),
        request_id: command.request_id.clone(),
        variant,
        artifact_reference: "image-1.bin".to_owned(),
        admitted_at: at(now),
        deadline: at(now + 600),
        generation: 1,
    }));
    store.write_atomic(write).await?;
    Ok(command)
}
pub async fn persist(store: &Store, result: CommandResult) -> Result<(), StorageError> {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.map(|_| ())
}
pub async fn dispatched(store: &Store, command: &Command<Value>, now: i64) {
    persist(store, result(command, CommandLifecycle::Dispatched, now))
        .await
        .unwrap();
}
pub fn artifact(signed: bool) -> FirmwareArtifact16 {
    FirmwareArtifact16 {
        artifact_reference: "image-1.bin".to_owned(),
        sha256: "ab".repeat(32),
        size_bytes: 4096,
        signed,
        test_only: true,
    }
}
pub fn job() -> FirmwareJob16 {
    FirmwareJob16 {
        revision: 0,
        state: FirmwareJobState16::Pending,
        deadline: at(0),
        observed_at: at(0),
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
    }
}
pub fn reply_result(command: &Command<Value>, reply: FirmwareReply16, now: i64) -> CommandResult {
    let evidence = match command.operation {
        CommandOperation::Ocpp(ref operation) if operation.action.as_str() == "UpdateFirmware" => {
            FirmwareResult16::UpdateFirmware {
                artifact: Some(artifact(false)),
                reply: Some(reply),
                job: job(),
            }
        }
        CommandOperation::Ocpp(ref operation) => FirmwareResult16::SignedUpdateFirmware {
            request_id: i32::try_from(operation.payload["requestId"].as_i64().unwrap()).unwrap(),
            artifact: Some(artifact(true)),
            reply: Some(reply),
            job: job(),
        },
        _ => panic!("firmware command"),
    };
    let accepted = evidence.accepted();
    let mut value = result(
        command,
        CommandLifecycle::ProtocolResponse {
            accepted,
            error: (!accepted).then_some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                detail: None,
            }),
        },
        now,
    );
    value.firmware_16 = Some(evidence);
    value.schema_version = ContractVersion::V1_FIRMWARE_16;
    value
}
pub async fn reply(store: &Store, command: &Command<Value>, reply: FirmwareReply16, now: i64) {
    persist(store, reply_result(command, reply, now))
        .await
        .unwrap();
}
pub fn signed_status(status: SignedUpdateFirmwareStatus16) -> FirmwareReply16 {
    FirmwareReply16::Status { status }
}
pub async fn notify(
    store: &Store,
    status: FirmwareStatus16,
    signed: bool,
    request_id: Option<i32>,
    now: i64,
) {
    observe(
        store,
        FirmwareObservationKind16::Status {
            status,
            signed,
            request_id,
        },
        now,
    )
    .await;
}
pub async fn observe(store: &Store, kind: FirmwareObservationKind16, now: i64) {
    let mut write = AtomicStoreWrite::empty();
    write.firmware_observations_16.push(FirmwareObservation16 {
        station: station(),
        observed_at: at(now),
        kind,
    });
    store.write_atomic(write).await.unwrap();
}
pub async fn jobs(store: &Store) -> Vec<FirmwareJobRecord16> {
    store.firmware_jobs_16(station()).await.unwrap()
}
pub async fn evidence(store: &Store, command: &Command<Value>) -> FirmwareResult16 {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
        .firmware_16
        .unwrap()
}
pub async fn state(store: &Store, command: &Command<Value>) -> FirmwareJobState16 {
    evidence(store, command).await.job().state
}
