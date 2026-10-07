use parking_lot::Mutex;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::future::poll_fn;
use tokio::time::{Instant, advance};
use uob_application::{
    AtomicStoreWrite, CommittedRecord, CommittedRecordId, ConfigurationError, ConfigurationSchema,
    DatabaseAcknowledgementScope, DatabaseConfiguration, DatabaseError, DatabaseErrorCode,
    DatabaseExportContext, DatabaseProvider, DatabaseProviderDescriptor, DatabaseProviderFactory,
    DatabaseProviderKind, DatabaseProviderLimits, DatabaseRetryClassification, DatabaseTask,
    DeduplicationCapability, Durability, ExportIngestor, ExportSpool, ExportSpoolNamespace,
    OperationalStore, RuntimeResourceBudget, RuntimeResourceLimits, StorageWritePurpose,
    TransactionCapability, ValidatedDatabaseConfiguration,
};
use uob_contracts::{
    ArtifactDigest, AvailabilityState, BridgeId, ContractVersion, Environment, ExportDestination,
    ExportDestinationId, ExportOutcome, ExportPayload, ExportRecord, ExportRecordId,
    ExportRecordIdentity, ExportRecordKind, ExportRecordMetadata, ExportResourceStatusChange,
    ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity, StationId, UtcTimestamp,
};
use uob_external_export_adapter::{
    ConfiguredDatabaseProvider, DataExportConfiguration, DatabaseProviderRegistration,
    DatabaseProviderRegistry, DatabaseTransportSecurity, DestinationTransition, ExportBacklogState,
    ExportScheduler,
};
use uob_storage_adapter::{ExportSpoolLimits, SqliteExportSpool, SqliteOperationalStore};
use uuid::Uuid;

pub(super) type Source = SqliteOperationalStore<String, String, String, ExportRecord>;

pub(super) struct Fixture {
    source_path: PathBuf,
    spool_path: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Option<Self> {
        if fs::metadata(std::env::temp_dir()).ok()?.dev() == fs::metadata("/dev/shm").ok()?.dev() {
            return None;
        }
        let unique = Uuid::new_v4();
        let spool_path = PathBuf::from(format!("/dev/shm/uob-scheduler-{unique}"));
        fs::create_dir(&spool_path).unwrap();
        fs::set_permissions(&spool_path, fs::Permissions::from_mode(0o700)).unwrap();
        Some(Self {
            source_path: std::env::temp_dir().join(format!("uob-scheduler-{unique}.sqlite3")),
            spool_path,
        })
    }

    pub(super) fn source(&self) -> Source {
        Source::open(&self.source_path, 8).unwrap()
    }

    pub(super) fn open(&self, resources: &RuntimeResourceBudget) -> (Source, SqliteExportSpool) {
        let source = Source::open(&self.source_path, 8).unwrap();
        let spool = SqliteExportSpool::open_with_limits(
            &self.spool_path,
            &self.source_path,
            8,
            ExportSpoolLimits::new(1024 * 1024, 8).unwrap(),
            resources,
        )
        .unwrap();
        (source, spool)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{}", self.source_path.display(), suffix));
        }
        let _ = fs::remove_file(self.spool_path.join("export.sqlite3"));
        let _ = fs::remove_dir(&self.spool_path);
    }
}

pub(super) fn resources() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap()
}

fn destination() -> ExportDestination {
    ExportDestination {
        destination_id: ExportDestinationId::new("analytics").unwrap(),
        configuration_revision: 7,
    }
}

pub(super) fn record(id: &str) -> CommittedRecord<ExportRecord> {
    let instant = UtcTimestamp::new(time::OffsetDateTime::now_utc());
    CommittedRecord {
        record_id: CommittedRecordId::new(id).unwrap(),
        durability: Durability::Critical,
        committed_at: instant,
        record: ExportRecord::new(
            ExportRecordMetadata {
                identity: ExportRecordIdentity::root(ExportRecordId::new(id).unwrap()),
                schema_version: ExportRecord::SCHEMA_VERSION,
                runtime: RuntimeIdentity {
                    environment: Environment::Demo,
                    release_id: ReleaseId::new("release").unwrap(),
                    release_digest: ArtifactDigest::new("sha256:test").unwrap(),
                    process_instance_id: ProcessInstanceId::new("instance").unwrap(),
                },
                resource: ResourceRef {
                    bridge_id: BridgeId::new("bridge").unwrap(),
                    station_id: StationId::new("station").unwrap(),
                    resource: None,
                    native_protocol_reference: None,
                },
                source_time: None,
                observed_at: instant,
                sequence: 1,
                correlation_id: None,
            },
            ExportPayload::ResourceStatusChange(ExportResourceStatusChange {
                previous: AvailabilityState::Unknown,
                current: AvailabilityState::Available,
            }),
        ),
    }
}

pub(super) async fn populated_spool(
    fixture: &Fixture,
    resources: &RuntimeResourceBudget,
) -> (Arc<SqliteExportSpool>, ExportSpoolNamespace) {
    populated_records(
        fixture,
        resources,
        vec![record("critical-1"), record("critical-2")],
    )
    .await
}

pub(super) async fn populated_records(
    fixture: &Fixture,
    resources: &RuntimeResourceBudget,
    records: Vec<CommittedRecord<ExportRecord>>,
) -> (Arc<SqliteExportSpool>, ExportSpoolNamespace) {
    let expected = records.len();
    let (source, spool) = fixture.open(resources);
    source
        .write_atomic(AtomicStoreWrite {
            charging_profile_201: None,
            reservation_16: None,
            reservation_observations_16: Vec::new(),
            reservation_201: None,
            reservation_observations_201: Vec::new(),
            firmware_16: None,
            firmware_observations_16: Vec::new(),
            firmware_201: None,
            diagnostics_16: None,
            diagnostics_201: None,
            firmware_observations_201: Vec::new(),
            diagnostics_observations_16: Vec::new(),
            diagnostics_observations_201: Vec::new(),
            purpose: StorageWritePurpose::Routine,
            station_snapshot: None,
            authorization_changes: vec![],
            command: None,
            command_result: None,
            journal_events: vec![],
            required_deliveries: vec![],
            committed_records: records,
        })
        .await
        .unwrap();
    let mut status =
        ExportIngestor::ingest_once(&source, &spool, destination(), "test.scheduler", resources)
            .await
            .unwrap()
            .status;
    while usize::try_from(status.pending_records).expect("pending record count exceeds usize")
        < expected
    {
        let prior = status.pending_records;
        status = ExportIngestor::ingest_once(
            &source,
            &spool,
            destination(),
            "test.scheduler",
            resources,
        )
        .await
        .unwrap()
        .status;
        assert!(
            status.pending_records > prior,
            "ingestion must advance pending records"
        );
    }
    assert_eq!(
        usize::try_from(status.pending_records).expect("pending record count exceeds usize"),
        expected
    );
    (
        Arc::new(spool),
        ExportSpoolNamespace {
            destination: destination(),
            provider_kind: "test.scheduler".into(),
            source_generation: status.source_generation,
        },
    )
}

// The spool runs on an OS thread. Wait for its observable result before moving
// Tokio's virtual clock so slow physical I/O cannot consume a virtual deadline.
pub(super) async fn until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "scheduler made no progress"
        );
        std::thread::sleep(Duration::from_millis(1));
        tokio::task::yield_now().await;
    }
}

pub(super) async fn confirm_after_retries(
    control: &Control,
    handle: &uob_external_export_adapter::ExportSchedulerHandle,
) {
    for attempt in 1..=3 {
        until(|| control.observations.lock().len() == attempt).await;
        if attempt < 3 {
            until(|| handle.health().retry_count == attempt as u64).await;
            advance(Duration::from_secs(1)).await;
        }
    }
    until(|| handle.health().confirmed_records == 2).await;
}

#[derive(Clone, Copy)]
pub(super) enum Behavior {
    Commit,
    FailTwice,
    CommitSlowShutdown,
    Hung,
}

#[derive(Clone)]
pub(super) struct Observation {
    pub(super) batch_id: String,
    pub(super) bytes: usize,
    pub(super) identities: Vec<String>,
    pub(super) records: usize,
    pub(super) at: Instant,
}

#[derive(Default)]
pub(super) struct Control {
    pub(super) attempts: AtomicUsize,
    pub(super) shutdown_seen: AtomicUsize,
    pub(super) open: AtomicUsize,
    pub(super) peak: AtomicUsize,
    pub(super) observations: Mutex<Vec<Observation>>,
    pub(super) limits: Mutex<Option<(usize, usize)>>,
}

struct SessionGuard(Arc<Control>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

struct FakeFactory {
    control: Arc<Control>,
    behavior: Behavior,
}
struct FakeProvider {
    control: Arc<Control>,
    behavior: Behavior,
    descriptor: DatabaseProviderDescriptor,
}

impl DatabaseProviderFactory for FakeFactory {
    fn kind(&self) -> &'static str {
        "test.scheduler"
    }
    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema { fields: vec![] }
    }
    fn validate(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<ValidatedDatabaseConfiguration, ConfigurationError> {
        Ok(ValidatedDatabaseConfiguration::new(configuration.clone()))
    }
    fn create(
        &self,
        _configuration: ValidatedDatabaseConfiguration,
    ) -> Result<Box<dyn DatabaseProvider>, ConfigurationError> {
        let limits = self.control.limits.lock().unwrap_or((100, 256 * 1024));
        Ok(Box::new(FakeProvider {
            control: self.control.clone(),
            behavior: self.behavior,
            descriptor: DatabaseProviderDescriptor {
                kind: DatabaseProviderKind::new("test.scheduler").unwrap(),
                instance_id: destination().destination_id,
                record_schema_versions: vec![
                    ContractVersion::V1_INITIAL,
                    ExportRecord::SCHEMA_VERSION,
                ],
                supported_record_classes: vec![ExportRecordKind::ResourceStatusChange],
                limits: DatabaseProviderLimits {
                    maximum_records_per_batch: limits.0,
                    maximum_batch_bytes: limits.1,
                    maximum_in_flight_batches: 1,
                },
                deduplication: DeduplicationCapability::StableRecordIdentity,
                transactions: TransactionCapability::AtomicBatch,
                acknowledgement_scope: DatabaseAcknowledgementScope::AtomicRemoteCommit,
            },
        }))
    }
}

impl DatabaseProvider for FakeProvider {
    fn descriptor(&self) -> DatabaseProviderDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, mut context: DatabaseExportContext) -> DatabaseTask {
        Box::pin(async move {
            let open = self.control.open.fetch_add(1, Ordering::SeqCst) + 1;
            self.control.peak.fetch_max(open, Ordering::SeqCst);
            let _guard = SessionGuard(self.control.clone());
            let batch = poll_fn(|cx| context.batches.as_mut().poll_receive(cx))
                .await
                .unwrap();
            self.descriptor.validate_batch(&batch)?;
            let attempt = self.control.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            self.control.observations.lock().push(Observation {
                batch_id: batch.batch_id().as_str().to_owned(),
                bytes: serde_json::to_vec(&batch).unwrap().len(),
                identities: batch
                    .records()
                    .iter()
                    .map(|record| record.metadata().identity.record_id.as_str().to_owned())
                    .collect(),
                records: batch.records().len(),
                at: Instant::now(),
            });
            if matches!(self.behavior, Behavior::Hung) {
                std::future::pending::<()>().await;
            }
            if matches!(self.behavior, Behavior::FailTwice) && attempt <= 2 {
                return Err(DatabaseError::new(
                    DatabaseErrorCode::ConnectionUnavailable,
                    DatabaseRetryClassification::Retryable,
                    "fake.outage",
                ));
            }
            let report =
                uob_contracts::ExportReport::for_batch(&batch, ExportOutcome::Committed).unwrap();
            context.critical_reports.report(report).await.unwrap();
            poll_fn(|cx| context.shutdown.as_mut().poll_shutdown(cx)).await;
            if matches!(self.behavior, Behavior::CommitSlowShutdown) {
                self.control.shutdown_seen.fetch_add(1, Ordering::SeqCst);
            }
            if matches!(self.behavior, Behavior::CommitSlowShutdown) {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}

fn selection(
    control: Arc<Control>,
    behavior: Behavior,
) -> uob_external_export_adapter::ValidatedDataExport {
    let mut registry = DatabaseProviderRegistry::new();
    registry
        .register(
            FakeFactory { control, behavior },
            DatabaseProviderRegistration {
                display_name: "Controlled provider".into(),
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
                    kind: "test.scheduler".into(),
                    configuration: DatabaseConfiguration::new(destination().destination_id, 7),
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

pub(super) fn start(
    control: &Arc<Control>,
    behavior: Behavior,
    namespace: &ExportSpoolNamespace,
    spool: &Arc<SqliteExportSpool>,
    resources: &RuntimeResourceBudget,
) -> uob_external_export_adapter::ExportSchedulerHandle {
    ExportScheduler::start(
        selection(control.clone(), behavior),
        namespace.clone(),
        spool.clone(),
        resources.clone(),
    )
    .unwrap()
    .unwrap()
}

pub(super) fn start_with_spool(
    control: &Arc<Control>,
    behavior: Behavior,
    namespace: &ExportSpoolNamespace,
    spool: Arc<dyn ExportSpool>,
    resources: &RuntimeResourceBudget,
) -> uob_external_export_adapter::ExportSchedulerHandle {
    ExportScheduler::start(
        selection(control.clone(), behavior),
        namespace.clone(),
        spool,
        resources.clone(),
    )
    .unwrap()
    .unwrap()
}
