use std::{
    collections::VecDeque,
    future::poll_fn,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use time::OffsetDateTime;
use uob_contracts::{
    ArtifactDigest, AvailabilityState, BridgeId, ContractVersion, Environment, ExportBatch,
    ExportBatchId, ExportDestination, ExportDestinationId, ExportErrorCode, ExportOutcome,
    ExportPayload, ExportRecord, ExportRecordId, ExportRecordIdentity, ExportRecordKind,
    ExportRecordMetadata, ExportRecordOutcome, ExportRecordOutcomeKind, ExportReport,
    ExportResourceStatusChange, ProcessInstanceId, ReleaseId, ResourceRef, RuntimeIdentity,
    StationId, UtcTimestamp,
};

use super::*;
use crate::{
    ConfigurationErrorCode, ConfigurationField, ConfigurationFieldKind, ConfigurationValue,
    CredentialReference,
};

fn batch_fixture() -> ExportBatch {
    let records = (0..2)
        .map(|ordinal| {
            ExportRecord::new(
                ExportRecordMetadata {
                    identity: ExportRecordIdentity::child(
                        ExportRecordId::new("event-status-42").expect("record id"),
                        ordinal,
                    ),
                    schema_version: ContractVersion::V1_INITIAL,
                    runtime: RuntimeIdentity {
                        environment: Environment::Demo,
                        release_id: ReleaseId::new("test-release").expect("release id"),
                        release_digest: ArtifactDigest::new("sha256:test").expect("digest"),
                        process_instance_id: ProcessInstanceId::new("test-process")
                            .expect("process id"),
                    },
                    resource: ResourceRef {
                        bridge_id: BridgeId::new("test-bridge").expect("bridge id"),
                        station_id: StationId::new("station-1").expect("station id"),
                        resource: None,
                        native_protocol_reference: None,
                    },
                    source_time: None,
                    observed_at: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
                    sequence: u64::from(ordinal),
                    correlation_id: None,
                },
                ExportPayload::ResourceStatusChange(ExportResourceStatusChange {
                    previous: AvailabilityState::Unknown,
                    current: AvailabilityState::Available,
                }),
            )
        })
        .collect();
    ExportBatch::new(
        ExportBatchId::new("batch-status-42").expect("batch id"),
        ExportDestination {
            destination_id: destination("analytics"),
            configuration_revision: 7,
        },
        records,
    )
    .expect("valid export batch fixture")
}

fn destination(value: &str) -> ExportDestinationId {
    ExportDestinationId::new(value).expect("valid destination")
}

fn descriptor(transactions: TransactionCapability) -> DatabaseProviderDescriptor {
    DatabaseProviderDescriptor {
        kind: DatabaseProviderKind::new("test.memory").expect("provider kind"),
        instance_id: destination("analytics"),
        record_schema_versions: vec![ContractVersion::V1_INITIAL],
        supported_record_classes: vec![ExportRecordKind::ResourceStatusChange],
        limits: DatabaseProviderLimits {
            maximum_records_per_batch: 10,
            maximum_batch_bytes: 32 * 1024,
            maximum_in_flight_batches: 1,
        },
        deduplication: DeduplicationCapability::StableRecordIdentity,
        transactions,
        acknowledgement_scope: match transactions {
            TransactionCapability::AtomicBatch => DatabaseAcknowledgementScope::AtomicRemoteCommit,
            TransactionCapability::PerRecord => DatabaseAcknowledgementScope::PerRecordRemoteCommit,
        },
    }
}

struct BoundedBatches {
    batches: VecDeque<ExportBatch>,
    capacity: usize,
}

impl DatabaseBatchReceiver for BoundedBatches {
    fn poll_receive(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<ExportBatch>> {
        Poll::Ready(self.batches.pop_front())
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn backlog(&self) -> usize {
        self.batches.len()
    }
}

#[derive(Default)]
struct HostPorts {
    reports: Mutex<Vec<ExportReport>>,
}

impl DatabaseReportPort for HostPorts {
    fn report(&self, report: ExportReport) -> DatabasePortFuture<'_> {
        Box::pin(async move {
            self.reports.lock().expect("report lock").push(report);
            Ok(())
        })
    }
}

impl DatabaseDiagnosticPort for HostPorts {
    fn try_emit(&self, _diagnostic: DatabaseDiagnostic) -> Result<(), DatabaseDiagnosticDrop> {
        Err(DatabaseDiagnosticDrop::Disabled)
    }
}

struct ImmediateShutdown;

impl DatabaseShutdown for ImmediateShutdown {
    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }
}

struct FakeProvider {
    runs: Arc<AtomicUsize>,
    descriptor: DatabaseProviderDescriptor,
}

impl DatabaseProvider for FakeProvider {
    fn descriptor(&self) -> DatabaseProviderDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, mut context: DatabaseExportContext) -> DatabaseTask {
        Box::pin(async move {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let batch = poll_fn(|cx| context.batches.as_mut().poll_receive(cx))
                .await
                .ok_or_else(|| {
                    DatabaseError::new(
                        DatabaseErrorCode::InvalidContext,
                        DatabaseRetryClassification::Permanent,
                        "context.batch_stream_closed",
                    )
                })?;
            self.descriptor.validate_batch(&batch)?;
            let report =
                ExportReport::for_batch(&batch, ExportOutcome::Committed).map_err(|_| {
                    DatabaseError::new(
                        DatabaseErrorCode::InvalidReport,
                        DatabaseRetryClassification::Permanent,
                        "report.construction",
                    )
                })?;
            self.descriptor.validate_report(&report)?;
            context.critical_reports.report(report).await.map_err(|_| {
                DatabaseError::new(
                    DatabaseErrorCode::CapacityExhausted,
                    DatabaseRetryClassification::Retryable,
                    "report.backpressure",
                )
            })
        })
    }
}

struct FakeFactory {
    runtime_connections: Arc<AtomicUsize>,
    runs: Arc<AtomicUsize>,
}

impl DatabaseProviderFactory for FakeFactory {
    fn kind(&self) -> &'static str {
        "test.memory"
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema {
            fields: vec![ConfigurationField {
                name: "credentials".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: true,
            }],
        }
    }

    fn validate(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<ValidatedDatabaseConfiguration, ConfigurationError> {
        assert_eq!(self.runtime_connections.load(Ordering::SeqCst), 0);
        match configuration.setting("credentials") {
            Some(ConfigurationValue::CredentialReference(_)) => {
                Ok(ValidatedDatabaseConfiguration::new(configuration.clone()))
            }
            Some(_) => Err(ConfigurationError::field(
                ConfigurationErrorCode::InvalidField,
                "credentials",
            )),
            None => Err(ConfigurationError::field(
                ConfigurationErrorCode::MissingField,
                "credentials",
            )),
        }
    }

    fn create(
        &self,
        _configuration: ValidatedDatabaseConfiguration,
    ) -> Result<Box<dyn DatabaseProvider>, ConfigurationError> {
        Ok(Box::new(FakeProvider {
            runs: Arc::clone(&self.runs),
            descriptor: descriptor(TransactionCapability::AtomicBatch),
        }))
    }
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn poll_ready(task: &mut DatabaseTask) -> Poll<Result<(), DatabaseError>> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    task.as_mut().poll(&mut context)
}

#[test]
fn fake_provider_runs_as_trait_object_with_only_a_bounded_batch_surface() {
    let runtime_connections = Arc::new(AtomicUsize::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let factory: Box<dyn DatabaseProviderFactory> = Box::new(FakeFactory {
        runtime_connections: Arc::clone(&runtime_connections),
        runs: Arc::clone(&runs),
    });
    let configuration = DatabaseConfiguration::new(destination("analytics"), 4).with_setting(
        "credentials",
        ConfigurationValue::CredentialReference(
            CredentialReference::new("vault://database/analytics").expect("credential reference"),
        ),
    );
    let validated = factory
        .validate(&configuration)
        .expect("valid configuration");
    let provider = factory.create(validated).expect("constructed provider");
    let host = Arc::new(HostPorts::default());
    let batch = batch_fixture();
    let mut task = provider.run(DatabaseExportContext {
        batches: Box::pin(BoundedBatches {
            batches: VecDeque::from([batch]),
            capacity: 1,
        }),
        critical_reports: host.clone(),
        diagnostics: host.clone(),
        limits: DatabaseRuntimeLimits {
            maximum_in_flight_batches: 1,
            maximum_records_per_batch: 10,
            maximum_batch_bytes: 32 * 1024,
        },
        shutdown: Box::pin(ImmediateShutdown),
        shutdown_deadline: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
    });

    assert_eq!(poll_ready(&mut task), Poll::Ready(Ok(())));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(runtime_connections.load(Ordering::SeqCst), 0);
    assert_eq!(host.reports.lock().expect("report lock").len(), 1);
}

#[test]
fn validation_is_network_free_and_never_reveals_connection_or_credential_text() {
    let runtime_connections = Arc::new(AtomicUsize::new(0));
    let factory = FakeFactory {
        runtime_connections: Arc::clone(&runtime_connections),
        runs: Arc::new(AtomicUsize::new(0)),
    };
    let secret = "driver://admin:secret@external.internal/bridge";
    let invalid = DatabaseConfiguration::new(destination("analytics"), 1)
        .with_setting("credentials", ConfigurationValue::Text(secret.to_owned()));
    let error = match factory.validate(&invalid) {
        Ok(_) => panic!("wrong credential type must fail"),
        Err(error) => error,
    };
    let diagnostic = format!("{:?} {error:?} {error}", factory.configuration_schema());

    assert!(!diagnostic.contains(secret));
    assert_eq!(error.code(), ConfigurationErrorCode::InvalidField);
    assert_eq!(runtime_connections.load(Ordering::SeqCst), 0);
}

#[test]
fn descriptors_reject_unsupported_records_and_overstated_transaction_outcomes() {
    let batch = batch_fixture();
    let mut atomic = descriptor(TransactionCapability::AtomicBatch);
    atomic.supported_record_classes = vec![ExportRecordKind::Measurement];
    let error = atomic
        .validate_batch(&batch)
        .expect_err("resource status unsupported");
    assert_eq!(error.code(), DatabaseErrorCode::UnsupportedRecordClass);

    atomic
        .supported_record_classes
        .push(ExportRecordKind::ResourceStatusChange);
    atomic.validate_batch(&batch).expect("supported batch");
    let partial = complete_partial_report(&batch);
    assert_eq!(
        atomic
            .validate_report(&partial)
            .expect_err("atomic provider cannot partially commit")
            .code(),
        DatabaseErrorCode::InvalidReport
    );

    let per_record = descriptor(TransactionCapability::PerRecord);
    per_record
        .validate_report(&partial)
        .expect("per-record provider reports every record");
    let committed =
        ExportReport::for_batch(&batch, ExportOutcome::Committed).expect("valid contract report");
    assert_eq!(
        per_record
            .validate_report(&committed)
            .expect_err("non-atomic provider cannot claim atomic commit")
            .code(),
        DatabaseErrorCode::InvalidReport
    );
}

fn complete_partial_report(batch: &ExportBatch) -> ExportReport {
    let records = batch
        .records()
        .iter()
        .map(|record| ExportRecordOutcome {
            identity: record.metadata().identity.clone(),
            result: if record
                .metadata()
                .identity
                .subrecord_id
                .expect("child")
                .ordinal()
                == 0
            {
                ExportRecordOutcomeKind::Committed
            } else {
                ExportRecordOutcomeKind::Permanent {
                    error: ExportErrorCode::new("unsupported_record").expect("error code"),
                }
            },
        })
        .collect();
    ExportReport::for_batch(batch, ExportOutcome::Partial { records })
        .expect("complete per-record report")
}
