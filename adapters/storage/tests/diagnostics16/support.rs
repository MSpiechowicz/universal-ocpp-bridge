use serde_json::{Value, json};
use std::path::PathBuf;
pub use uob_application::*;
pub use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, String, String, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-diagnostics16-{}.db", uuid::Uuid::new_v4())))
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
pub const LEGACY: DiagnosticsVariant16 = DiagnosticsVariant16::Diagnostics;
pub const fn security(request_id: i32) -> DiagnosticsVariant16 {
    DiagnosticsVariant16::Log {
        log_type: LogType16::SecurityLog,
        request_id,
    }
}
pub fn command(id: &str, variant: DiagnosticsVariant16, now: i64) -> Command<Value> {
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
        DiagnosticsVariant16::Diagnostics => (
            "GetDiagnostics",
            GET_DIAGNOSTICS_REFERENCE_SCHEMA_16,
            json!({"startTime":at(0),"retries":2}),
        ),
        DiagnosticsVariant16::Log {
            log_type,
            request_id,
        } => (
            "GetLog",
            GET_LOG_REFERENCE_SCHEMA_16,
            json!({"logType":log_type,"requestId":request_id}),
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
    variant: DiagnosticsVariant16,
    now: i64,
) -> Result<Command<Value>, StorageError> {
    let command = command(id, variant, now);
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted, now));
    write.diagnostics_16 = Some(Box::new(DiagnosticsJobMutation16 {
        station: station(),
        request_id: command.request_id.clone(),
        variant,
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
/// Dispatched lifecycle plus the destination binding the session performs before sending.
pub async fn dispatched(store: &Store, command: &Command<Value>, now: i64) {
    persist(store, result(command, CommandLifecycle::Dispatched, now))
        .await
        .unwrap();
    store
        .bind_diagnostics_upload_16(command.request_id.clone(), upload_id(command))
        .await
        .unwrap();
}
pub fn upload_id(command: &Command<Value>) -> String {
    format!("upload-{}", command.request_id.as_str())
}
pub fn destination(variant: DiagnosticsVariant16) -> DiagnosticsDestination16 {
    DiagnosticsDestination16 {
        log_type: variant.log_type(),
        maximum_bytes: 4096,
        test_only: true,
    }
}
pub fn job() -> DiagnosticsJob16 {
    DiagnosticsJob16 {
        revision: 0,
        state: DiagnosticsJobState16::Pending,
        deadline: at(0),
        observed_at: at(0),
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
        upload: None,
    }
}
pub fn variant_of(command: &Command<Value>) -> DiagnosticsVariant16 {
    match command.operation {
        CommandOperation::Ocpp(ref operation) if operation.action.as_str() == "GetDiagnostics" => {
            LEGACY
        }
        CommandOperation::Ocpp(ref operation) => DiagnosticsVariant16::Log {
            log_type: serde_json::from_value(operation.payload["logType"].clone()).unwrap(),
            request_id: i32::try_from(operation.payload["requestId"].as_i64().unwrap()).unwrap(),
        },
        _ => panic!("diagnostics command"),
    }
}
pub fn reply_result(
    command: &Command<Value>,
    reply: DiagnosticsReply16,
    now: i64,
) -> CommandResult {
    let variant = variant_of(command);
    let evidence = match variant {
        DiagnosticsVariant16::Diagnostics => DiagnosticsResult16::GetDiagnostics {
            destination: Some(destination(variant)),
            reply: Some(reply),
            job: job(),
        },
        DiagnosticsVariant16::Log {
            log_type,
            request_id,
        } => DiagnosticsResult16::GetLog {
            log_type,
            request_id,
            destination: Some(destination(variant)),
            reply: Some(reply),
            job: job(),
        },
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
    value.diagnostics_16 = Some(evidence);
    value.schema_version = ContractVersion::V1_DIAGNOSTICS_16;
    value
}
pub async fn reply(store: &Store, command: &Command<Value>, reply: DiagnosticsReply16, now: i64) {
    persist(store, reply_result(command, reply, now))
        .await
        .unwrap();
}
pub fn file(name: &str) -> DiagnosticsReply16 {
    DiagnosticsReply16::Diagnostics {
        file_name: Some(name.to_owned()),
    }
}
pub fn log_status(status: GetLogStatus16) -> DiagnosticsReply16 {
    DiagnosticsReply16::Log {
        status,
        file_name: Some("security.log".to_owned()),
    }
}
pub fn stored(command: &Command<Value>) -> UploadCheck16 {
    UploadCheck16 {
        upload_id: upload_id(command),
        outcome: UploadOutcome16::Received(DiagnosticsUpload16 {
            sha256: "cd".repeat(32),
            size_bytes: 777,
        }),
    }
}
pub async fn notify(
    store: &Store,
    status: LogUploadStatus16,
    log: bool,
    request_id: Option<i32>,
    upload: Option<UploadCheck16>,
    now: i64,
) {
    observe(
        store,
        DiagnosticsObservationKind16::Status {
            status,
            log,
            request_id,
            upload,
        },
        now,
    )
    .await;
}
pub async fn observe(store: &Store, kind: DiagnosticsObservationKind16, now: i64) {
    let mut write = AtomicStoreWrite::empty();
    write
        .diagnostics_observations_16
        .push(DiagnosticsObservation16 {
            station: station(),
            observed_at: at(now),
            kind,
        });
    store.write_atomic(write).await.unwrap();
}
pub async fn jobs(store: &Store) -> Vec<DiagnosticsJobRecord16> {
    store.diagnostics_jobs_16(station()).await.unwrap()
}
pub async fn evidence(store: &Store, command: &Command<Value>) -> DiagnosticsResult16 {
    store
        .command_result_by_request_id(command.request_id.clone())
        .await
        .unwrap()
        .unwrap()
        .diagnostics_16
        .unwrap()
}
pub async fn state(store: &Store, command: &Command<Value>) -> DiagnosticsJobState16 {
    evidence(store, command).await.job().state
}
