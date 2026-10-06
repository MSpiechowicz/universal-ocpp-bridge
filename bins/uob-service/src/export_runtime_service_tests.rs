#![cfg(target_os = "linux")]

#[path = "export_runtime_service_scenarios.rs"]
mod scenarios;

#[path = "export_runtime_service_command.rs"]
mod command;

use std::{
    fs,
    net::TcpListener,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uob_application::{
    AtomicStoreWrite, CommittedRecord, CommittedRecordId, ConfigurationError, ConfigurationSchema,
    DatabaseAcknowledgementScope, DatabaseConfiguration, DatabaseError, DatabaseErrorCode,
    DatabaseExportContext, DatabaseProvider, DatabaseProviderDescriptor, DatabaseProviderFactory,
    DatabaseProviderKind, DatabaseProviderLimits, DatabaseRetryClassification, DatabaseTask,
    DeduplicationCapability, Durability, ExportIngestor, ExportSpoolNamespace, OperationalStore,
    StorageWritePurpose, TransactionCapability, ValidatedDatabaseConfiguration,
};
use uob_contracts::{
    AvailabilityState, ContractVersion, Environment, ExportDestination, ExportDestinationId,
    ExportPayload, ExportRecord, ExportRecordId, ExportRecordIdentity, ExportRecordKind,
    ExportRecordMetadata, ExportResourceStatusChange, ResourceRef, StationId, UtcTimestamp,
};
use uob_external_export_adapter::{
    ConfiguredDatabaseProvider, DataExportConfiguration, DatabaseProviderRegistration,
    DatabaseProviderRegistry, DatabaseTransportSecurity, DestinationTransition, ExportBacklogState,
};
use uob_storage_adapter::{SqliteExportSpool, SqliteOperationalStore};

const READ_TOKEN: &str = "uob1.demo.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PRIVILEGED_TOKEN: &str = "uob1.demo.cccccccccccccccccccccccccccccccc";
const ALPHA: &str = "c3RhdGlvbi1hOnN0YXRpb24tYWxwaGEtc2VjcmV0LTEyMzQ1";
type Source = SqliteOperationalStore<String, String, String, ExportRecord>;

struct Fixture {
    root: PathBuf,
    spool_directory: PathBuf,
    document: PathBuf,
    source_path: PathBuf,
    management: u16,
    charging: u16,
}

impl Fixture {
    fn new() -> Option<Self> {
        let root =
            std::env::temp_dir().join(format!("uob-export-service-{}", uuid::Uuid::new_v4()));
        let spool_directory =
            PathBuf::from("/dev/shm").join(format!("uob-export-service-{}", uuid::Uuid::new_v4()));
        if fs::metadata(std::env::temp_dir()).ok()?.dev() == fs::metadata("/dev/shm").ok()?.dev() {
            return None;
        }
        fs::create_dir(&root).unwrap();
        fs::create_dir(&spool_directory).unwrap();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        for dir in [&root, &spool_directory, &state] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        for (name, value) in [
            ("read-grant", READ_TOKEN),
            (
                "control-grant",
                "uob1.demo.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
            ("privileged-grant", PRIVILEGED_TOKEN),
            ("start-token", "TEST-TAG-12345678"),
            ("station-a", "station-alpha-secret-12345"),
        ] {
            let path = root.join(name);
            fs::write(&path, value).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let management = vacant_port();
        let mut charging = vacant_port();
        while charging == management {
            charging = vacant_port();
        }
        let document = root.join("bridge.toml");
        fs::write(
            &document,
            format!(
                "[bridge]\nid='bridge-1'\nenvironment='demo'\n\
             [management]\nlisten_addr='127.0.0.1:{management}'\n\
             [charging]\nenabled=true\nlisten_addr='127.0.0.1:{charging}'\n\
             state_directory='{}'\nread_grant_file='{}'\ncontrol_grant_file='{}'\nprivileged_grant_file='{}'\n\
             [[charging.stations]]\nid='station-a'\nprotocol='ocpp16j'\ntrigger_message=true\ncredential_file='{}'\nstart_token_file='{}'\nallow_stop=true\n\
             [[charging.stations.resources]]\nconnector_id='connector-1'\nnative_connector_id=1\n",
                state.display(),
                root.join("read-grant").display(),
                root.join("control-grant").display(),
                root.join("privileged-grant").display(),
                root.join("station-a").display(),
                root.join("start-token").display()
            ),
        )
        .unwrap();
        Some(Self {
            source_path: root.join("source.sqlite3"),
            root,
            spool_directory,
            document,
            management,
            charging,
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.spool_directory);
    }
}

fn vacant_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct FixtureFactory {
    attempts: Arc<AtomicUsize>,
    hung: bool,
    active: Arc<AtomicUsize>,
}

impl DatabaseProviderFactory for FixtureFactory {
    fn kind(&self) -> &'static str {
        "test.export"
    }
    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema { fields: vec![] }
    }
    fn validate(
        &self,
        config: &DatabaseConfiguration,
    ) -> Result<ValidatedDatabaseConfiguration, ConfigurationError> {
        Ok(ValidatedDatabaseConfiguration::new(config.clone()))
    }
    fn create(
        &self,
        _: ValidatedDatabaseConfiguration,
    ) -> Result<Box<dyn DatabaseProvider>, ConfigurationError> {
        Ok(Box::new(FixtureProvider {
            attempts: self.attempts.clone(),
            hung: self.hung,
            active: self.active.clone(),
        }))
    }
}

struct FixtureProvider {
    attempts: Arc<AtomicUsize>,
    hung: bool,
    active: Arc<AtomicUsize>,
}

impl DatabaseProvider for FixtureProvider {
    fn descriptor(&self) -> DatabaseProviderDescriptor {
        DatabaseProviderDescriptor {
            kind: DatabaseProviderKind::new("test.export").unwrap(),
            instance_id: ExportDestinationId::new("analytics").unwrap(),
            record_schema_versions: vec![ContractVersion::V1_INITIAL, ExportRecord::SCHEMA_VERSION],
            supported_record_classes: vec![ExportRecordKind::ResourceStatusChange],
            limits: DatabaseProviderLimits {
                maximum_records_per_batch: 100,
                maximum_batch_bytes: 256 * 1024,
                maximum_in_flight_batches: 1,
            },
            deduplication: DeduplicationCapability::StableRecordIdentity,
            transactions: TransactionCapability::AtomicBatch,
            acknowledgement_scope: DatabaseAcknowledgementScope::AtomicRemoteCommit,
        }
    }
    fn run(self: Box<Self>, _context: DatabaseExportContext) -> DatabaseTask {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.hung {
            self.active.fetch_add(1, Ordering::SeqCst);
            let session = HungSession(self.active);
            Box::pin(async move {
                let _session = session;
                std::future::pending().await
            })
        } else {
            Box::pin(async {
                Err(DatabaseError::new(
                    DatabaseErrorCode::ConnectionUnavailable,
                    DatabaseRetryClassification::Retryable,
                    "fixture.outage",
                ))
            })
        }
    }
}

struct HungSession(Arc<AtomicUsize>);

impl Drop for HungSession {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn destination() -> ExportDestination {
    ExportDestination {
        destination_id: ExportDestinationId::new("analytics").unwrap(),
        configuration_revision: 1,
    }
}

async fn pending_export(
    fixture: &Fixture,
    application: &uob_application::Application,
) -> (Arc<SqliteExportSpool>, ExportSpoolNamespace) {
    let source = Source::open(&fixture.source_path, 8).unwrap();
    let spool = Arc::new(
        SqliteExportSpool::open(
            &fixture.spool_directory,
            &fixture.source_path,
            8,
            application.health().resources(),
        )
        .unwrap(),
    );
    let observed = UtcTimestamp::new(time::OffsetDateTime::now_utc());
    let record = ExportRecord::new(
        ExportRecordMetadata {
            identity: ExportRecordIdentity::root(ExportRecordId::new("service-export-1").unwrap()),
            schema_version: ExportRecord::SCHEMA_VERSION,
            runtime: application.runtime_identity().clone(),
            resource: ResourceRef {
                bridge_id: application.identity().bridge_id.clone(),
                station_id: StationId::new("station-a").unwrap(),
                resource: None,
                native_protocol_reference: None,
            },
            source_time: None,
            observed_at: observed,
            sequence: 1,
            correlation_id: None,
        },
        ExportPayload::ResourceStatusChange(ExportResourceStatusChange {
            previous: AvailabilityState::Unknown,
            current: AvailabilityState::Available,
        }),
    );
    source
        .write_atomic(AtomicStoreWrite {
            charging_profile_201: None,
            reservation_16: None,
            reservation_observations_16: Vec::new(),
            reservation_201: None,
            reservation_observations_201: Vec::new(),
            purpose: StorageWritePurpose::Routine,
            station_snapshot: None,
            authorization_changes: vec![],
            command: None,
            command_result: None,
            journal_events: vec![],
            required_deliveries: vec![],
            committed_records: vec![CommittedRecord {
                record_id: CommittedRecordId::new("service-export-1").unwrap(),
                durability: Durability::Critical,
                committed_at: observed,
                record,
            }],
        })
        .await
        .unwrap();
    let ingested = ExportIngestor::ingest_once(
        &source,
        spool.as_ref(),
        destination(),
        "test.export",
        application.health().resources(),
    )
    .await
    .unwrap();
    assert_eq!(ingested.status.pending_records, 1);
    (
        spool,
        ExportSpoolNamespace {
            destination: destination(),
            provider_kind: "test.export".into(),
            source_generation: ingested.status.source_generation,
        },
    )
}

fn selected(
    attempts: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    hung: bool,
) -> uob_external_export_adapter::ValidatedDataExport {
    let mut registry = DatabaseProviderRegistry::new();
    registry
        .register(
            FixtureFactory {
                attempts,
                hung,
                active,
            },
            DatabaseProviderRegistration {
                display_name: "Test export".into(),
            },
        )
        .unwrap();
    registry
        .validate(
            Environment::Demo,
            DataExportConfiguration {
                enabled: true,
                provider_id: Some(destination().destination_id.clone()),
                providers: vec![ConfiguredDatabaseProvider {
                    kind: "test.export".into(),
                    configuration: DatabaseConfiguration::new(destination().destination_id, 1),
                    transport_security: DatabaseTransportSecurity {
                        tls: false,
                        certificate_verification: false,
                        credentials_file: None,
                        explicitly_isolated: true,
                    },
                }],
            },
            &ExportBacklogState::default(),
            DestinationTransition::Preserve,
        )
        .unwrap()
}

async fn ocpp_charging_session(port: u16, label: &str, meter_start: i32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("charging listener ready");
    let mut request = format!("ws://127.0.0.1:{port}/ocpp/station-a")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    request
        .headers_mut()
        .insert("Authorization", format!("Basic {ALPHA}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let boot = ocpp_call(
        &mut socket,
        serde_json::json!([
            2, format!("{label}-boot"), "BootNotification",
            {"chargePointVendor": "TestVendor", "chargePointModel": "TestModel"}
        ]),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");
    let heartbeat = ocpp_call(
        &mut socket,
        serde_json::json!([2, format!("{label}-heartbeat"), "Heartbeat", {}]),
    )
    .await;
    assert_eq!(heartbeat[0], 3);

    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let started = ocpp_call(
        &mut socket,
        serde_json::json!([
            2, format!("{label}-start"), "StartTransaction",
            {"connectorId": 1, "idTag": "TEST-TAG-12345678", "meterStart": meter_start, "timestamp": timestamp}
        ]),
    )
    .await;
    assert_eq!(started[2]["idTagInfo"]["status"], "Accepted");
    let transaction_id = started[2]["transactionId"]
        .as_i64()
        .expect("transaction id");
    let stopped = ocpp_call(
        &mut socket,
        serde_json::json!([
            2, format!("{label}-stop"), "StopTransaction",
            {"transactionId": transaction_id, "meterStop": meter_start + 10, "timestamp": timestamp}
        ]),
    )
    .await;
    assert_eq!(stopped[0], 3);
    assert_eq!(stopped[1], format!("{label}-stop"));
}

async fn ocpp_call(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request: serde_json::Value,
) -> serde_json::Value {
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = response else {
        panic!("expected OCPP response");
    };
    let response: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(response[0], 3, "OCPP call failed: {response}");
    response
}
