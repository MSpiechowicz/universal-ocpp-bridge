use serde_json::{Value, json};
use std::path::PathBuf;
pub use uob_application::*;
pub use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, String, String, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-firmware201-{}.db", uuid::Uuid::new_v4())))
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
pub fn command(id: &str, request_id: i32, now: i64) -> Command<Value> {
    let mut value: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    value.request_id = RequestId::new(id).unwrap();
    value.correlation_id = Some(CorrelationId::new(id).unwrap());
    value.resource = station();
    value.admitted_at = at(now);
    value.expires_at = at(now + 1000);
    value.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("UpdateFirmware").unwrap(),
        payload_schema: PayloadSchemaId::new(UPDATE_FIRMWARE_REFERENCE_SCHEMA_201).unwrap(),
        payload: json!({
            "requestId": request_id,
            "artifactReference": "image-1.bin",
            "retrieveDateTime": at(now),
            "retries": 2
        }),
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
pub async fn admit_mode(
    store: &Store,
    id: &str,
    request_id: i32,
    secure: bool,
    now: i64,
) -> Result<Command<Value>, StorageError> {
    let command = command(id, request_id, now);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted, now));
    write.firmware_201 = Some(Box::new(FirmwareJobMutation201 {
        station: station(),
        request_id: command.request_id.clone(),
        native_request_id: request_id,
        secure,
        artifact_reference: "image-1.bin".to_owned(),
        admitted_at: at(now),
        deadline: at(now + 600),
        generation: 1,
    }));
    store.write_atomic(write).await?;
    Ok(command)
}
pub async fn admit(
    store: &Store,
    id: &str,
    request_id: i32,
    now: i64,
) -> Result<Command<Value>, StorageError> {
    admit_mode(store, id, request_id, true, now).await
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
pub fn artifact(signed: bool) -> FirmwareArtifact201 {
    FirmwareArtifact201 {
        artifact_reference: "image-1.bin".to_owned(),
        sha256: "ab".repeat(32),
        size_bytes: 4096,
        signed,
        test_only: true,
    }
}
pub fn job() -> FirmwareJob201 {
    FirmwareJob201 {
        revision: 0,
        state: FirmwareJobState201::Pending,
        deadline: at(0),
        observed_at: at(0),
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
    }
}
pub fn native_request_id(command: &Command<Value>) -> i32 {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        panic!("firmware command");
    };
    i32::try_from(operation.payload["requestId"].as_i64().unwrap()).unwrap()
}
pub fn reply_result(command: &Command<Value>, reply: FirmwareReply201, now: i64) -> CommandResult {
    let evidence = FirmwareResult201 {
        request_id: native_request_id(command),
        secure: true,
        artifact: Some(artifact(true)),
        reply: Some(reply),
        job: job(),
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
    value.firmware_201 = Some(evidence);
    value.schema_version = ContractVersion::V1_FIRMWARE_201;
    value
}
pub async fn reply(store: &Store, command: &Command<Value>, reply: FirmwareReply201, now: i64) {
    persist(store, reply_result(command, reply, now))
        .await
        .unwrap();
}
pub fn status(status: UpdateFirmwareStatus201) -> FirmwareReply201 {
    FirmwareReply201::Status {
        status,
        reason_code: None,
    }
}
pub async fn notify(store: &Store, status: FirmwareStatus201, request_id: Option<i32>, now: i64) {
    observe(
        store,
        FirmwareObservationKind201::Status { status, request_id },
        now,
    )
    .await;
}
pub async fn observe(store: &Store, kind: FirmwareObservationKind201, now: i64) {
    let mut write = AtomicStoreWrite::empty();
    write
        .firmware_observations_201
        .push(FirmwareObservation201 {
            station: station(),
            observed_at: at(now),
            kind,
        });
    store.write_atomic(write).await.unwrap();
}
pub async fn jobs(store: &Store) -> Vec<FirmwareJobRecord201> {
    store.firmware_jobs_201(station()).await.unwrap()
}
pub async fn evidence(store: &Store, command: &Command<Value>) -> FirmwareResult201 {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
        .firmware_201
        .unwrap()
}
pub async fn state(store: &Store, command: &Command<Value>) -> FirmwareJobState201 {
    evidence(store, command).await.job.state
}
