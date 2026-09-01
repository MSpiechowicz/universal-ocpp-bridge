use std::{
    collections::{BTreeMap, VecDeque},
    future::poll_fn,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use time::OffsetDateTime;
use tokio::time::{Duration, timeout};
use uob_application::{
    ConfigurationError, ConfigurationErrorCode, ConfigurationField, ConfigurationFieldKind,
    ConfigurationSchema, ConfigurationValue, DatabaseAcknowledgementScope, DatabaseConfiguration,
    DatabaseError, DatabaseErrorCode, DatabaseHealth, DatabaseHealthState, DatabaseProvider,
    DatabaseProviderDescriptor, DatabaseProviderFactory, DatabaseProviderKind,
    DatabaseProviderLimits, DatabaseRetryClassification, DatabaseRuntimeLimits, DatabaseTask,
    DeduplicationCapability, TransactionCapability, ValidatedDatabaseConfiguration,
};
use uob_contracts::{
    ContractVersion, ExportBatch, ExportErrorCode, ExportOutcome, ExportRecordKind,
    ExportRecordOutcome, ExportRecordOutcomeKind, ExportReport, ExportUncertainStage, UtcTimestamp,
};
use uob_database_conformance::{
    DescriptorViolation, ExportRecoveryLedger, FakeDatabaseHost, HostCapacities, HostError,
    RecoveryDisposition, inspect_descriptor,
};

const SECRET: &str = "postgres://operator:do-not-log@database.internal/charging";

#[derive(Default)]
struct ProviderState {
    connections: AtomicUsize,
    canonical_records: Mutex<BTreeMap<String, String>>,
}

struct ReferenceProvider {
    descriptor: DatabaseProviderDescriptor,
    revision: u64,
    outcomes: Mutex<VecDeque<ExportOutcome>>,
    state: Arc<ProviderState>,
}

impl DatabaseProvider for ReferenceProvider {
    fn descriptor(&self) -> DatabaseProviderDescriptor {
        self.descriptor.clone()
    }

    fn run(self: Box<Self>, mut context: uob_application::DatabaseExportContext) -> DatabaseTask {
        Box::pin(async move {
            if context.limits.maximum_in_flight_batches
                > self.descriptor.limits.maximum_in_flight_batches
            {
                return Err(failure(
                    DatabaseErrorCode::InvalidContext,
                    "context.in_flight_limit",
                ));
            }
            self.state.connections.fetch_add(1, Ordering::SeqCst);
            let _guard = ConnectionGuard(Arc::clone(&self.state));
            let _ = context
                .diagnostics
                .try_emit(uob_application::DatabaseDiagnostic::Health(
                    DatabaseHealth {
                        state: DatabaseHealthState::Ready,
                        batch_backlog: context.batches.backlog(),
                        in_flight_batches: 0,
                        active_connections: 1,
                        reason: Some("provider.ready".to_owned()),
                    },
                ));
            loop {
                tokio::select! {
                    batch = poll_fn(|cx| context.batches.as_mut().poll_receive(cx)) => {
                        let Some(batch) = batch else { continue };
                        self.descriptor.validate_batch(&batch)?;
                        if batch.destination().configuration_revision != self.revision {
                            return Err(failure(
                                DatabaseErrorCode::InvalidBatch,
                                "batch.configuration_revision",
                            ));
                        }
                        let outcome = self.outcomes.lock().expect("outcomes").pop_front()
                            .unwrap_or(ExportOutcome::Committed);
                        if matches!(outcome, ExportOutcome::Committed) {
                            commit_canonical(&self.state, &batch)?;
                        }
                        let report = ExportReport::for_batch(&batch, outcome).map_err(|_| {
                            failure(DatabaseErrorCode::InvalidReport, "report.construction")
                        })?;
                        self.descriptor.validate_report(&report)?;
                        context.critical_reports.report(report).await.map_err(|_| {
                            failure(DatabaseErrorCode::CapacityExhausted, "report.backpressure")
                        })?;
                    }
                    () = poll_fn(|cx| context.shutdown.as_mut().poll_shutdown(cx)) => return Ok(()),
                }
            }
        })
    }
}

struct ConnectionGuard(Arc<ProviderState>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

fn commit_canonical(state: &ProviderState, batch: &ExportBatch) -> Result<(), DatabaseError> {
    let mut stored = state.canonical_records.lock().expect("canonical records");
    for record in batch.records() {
        let key = format!(
            "{}:{:?}",
            record.metadata().identity.record_id,
            record.metadata().identity.subrecord_id
        );
        let canonical = serde_json::to_string(record)
            .map_err(|_| failure(DatabaseErrorCode::InvalidBatch, "record.serialization"))?;
        if stored
            .get(&key)
            .is_some_and(|existing| existing != &canonical)
        {
            return Err(failure(
                DatabaseErrorCode::InvalidBatch,
                "record.identity_conflict",
            ));
        }
        stored.insert(key, canonical);
    }
    Ok(())
}

fn failure(code: DatabaseErrorCode, context: &'static str) -> DatabaseError {
    DatabaseError::new(code, DatabaseRetryClassification::Permanent, context)
}

fn descriptor() -> DatabaseProviderDescriptor {
    DatabaseProviderDescriptor {
        kind: DatabaseProviderKind::new("test.archive").expect("provider kind"),
        instance_id: uob_contracts::ExportDestinationId::new("analytics").expect("destination"),
        record_schema_versions: vec![ContractVersion::V1_INITIAL],
        supported_record_classes: vec![
            ExportRecordKind::Measurement,
            ExportRecordKind::PointChange,
        ],
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

fn provider(
    state: Arc<ProviderState>,
    outcomes: impl IntoIterator<Item = ExportOutcome>,
) -> Box<dyn DatabaseProvider> {
    Box::new(ReferenceProvider {
        descriptor: descriptor(),
        revision: 7,
        outcomes: Mutex::new(outcomes.into_iter().collect()),
        state,
    })
}

fn fixture(batch_id: &str, revision: u64) -> ExportBatch {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../crates/contracts/tests/fixtures/export-batch-v1.json"
    ))
    .expect("fixture JSON");
    value["batch_id"] = serde_json::Value::String(batch_id.to_owned());
    value["destination"]["configuration_revision"] = revision.into();
    serde_json::from_value(value).expect("canonical fixture")
}

fn host() -> uob_database_conformance::HostContext {
    FakeDatabaseHost::build(
        HostCapacities {
            batches: 1,
            reports: 1,
            diagnostics: 1,
        },
        DatabaseRuntimeLimits {
            maximum_in_flight_batches: 1,
            maximum_records_per_batch: 100,
            maximum_batch_bytes: 256 * 1024,
        },
        UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10)),
    )
    .expect("bounded fake host")
}

#[tokio::test]
async fn replacement_provider_passes_batch_deduplication_outage_and_shutdown_contract() {
    let state = Arc::new(ProviderState::default());
    let adapter = provider(Arc::clone(&state), []);
    assert!(inspect_descriptor(&adapter.descriptor()).is_empty());
    let mut fixture_host = host();
    let first = fixture("batch-first", 7);
    fixture_host
        .host
        .try_send(first.clone())
        .expect("first batch");
    assert_eq!(
        fixture_host.host.try_send(fixture("batch-overflow", 7)),
        Err(HostError::Full)
    );
    let task = tokio::spawn(adapter.run(fixture_host.context));
    let ready = within(fixture_host.host.next_diagnostic())
        .await
        .expect("ready diagnostic");
    assert!(matches!(
        ready,
        uob_application::DatabaseDiagnostic::Health(DatabaseHealth {
            state: DatabaseHealthState::Ready,
            active_connections: 1,
            ..
        })
    ));
    let first_report = within(fixture_host.host.next_report())
        .await
        .expect("first report");
    assert!(first_report.is_fully_committed());
    assert_eq!(fixture_host.host.pending_batches(), 0);

    fixture_host
        .host
        .try_send(first)
        .expect("at-least-once retry");
    let retry_report = within(fixture_host.host.next_report())
        .await
        .expect("retry report");
    assert!(retry_report.is_fully_committed());
    assert_eq!(
        state.canonical_records.lock().expect("records").len(),
        2,
        "stable identities deduplicate the retry"
    );

    fixture_host.host.request_shutdown();
    assert!(within(task).await.expect("join").is_ok());
    assert_eq!(state.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unsupported_records_and_configuration_revisions_fail_instead_of_silent_acknowledgement() {
    let state = Arc::new(ProviderState::default());
    let mut unsupported = descriptor();
    unsupported.supported_record_classes = vec![ExportRecordKind::Measurement];
    let adapter: Box<dyn DatabaseProvider> = Box::new(ReferenceProvider {
        descriptor: unsupported,
        revision: 7,
        outcomes: Mutex::new(VecDeque::new()),
        state: Arc::clone(&state),
    });
    let mut fixture_host = host();
    fixture_host
        .host
        .try_send(fixture("batch-unsupported", 7))
        .expect("batch");
    let result = within(adapter.run(fixture_host.context))
        .await
        .expect_err("unsupported class must terminate explicitly");
    assert_eq!(result.code(), DatabaseErrorCode::UnsupportedRecordClass);
    assert!(fixture_host.host.next_report().await.is_none());

    let adapter = provider(state, []);
    let fixture_host = host();
    fixture_host
        .host
        .try_send(fixture("batch-stale", 6))
        .expect("batch");
    let result = within(adapter.run(fixture_host.context))
        .await
        .expect_err("stale revision must fail");
    assert_eq!(result.code(), DatabaseErrorCode::InvalidBatch);
    assert_eq!(result.context(), "batch.configuration_revision");
}

#[test]
fn broken_descriptors_fail_false_acknowledgement_and_bounded_resource_checks() {
    let mut broken = descriptor();
    broken.record_schema_versions.clear();
    broken.supported_record_classes.clear();
    broken.deduplication = DeduplicationCapability::None;
    broken.acknowledgement_scope =
        DatabaseAcknowledgementScope::TransportHandoff("socket.write".to_owned());
    broken.limits = DatabaseProviderLimits {
        maximum_records_per_batch: usize::MAX,
        maximum_batch_bytes: usize::MAX,
        maximum_in_flight_batches: usize::MAX,
    };

    assert_eq!(
        inspect_descriptor(&broken),
        vec![
            DescriptorViolation::MissingSchemaSurface,
            DescriptorViolation::MissingRecordSurface,
            DescriptorViolation::MissingStableDeduplication,
            DescriptorViolation::FalseCommitAcknowledgement,
            DescriptorViolation::InvalidRecordLimit,
            DescriptorViolation::InvalidByteLimit,
            DescriptorViolation::InvalidConcurrencyLimit,
        ]
    );
}

#[test]
fn explicit_outcomes_drive_checkpoint_retry_quarantine_and_reconciliation() {
    let batch = fixture("batch-outcome", 7);
    let error = ExportErrorCode::new("provider.outage").expect("error code");
    let mut ledger = ExportRecoveryLedger::default();
    for (outcome, expected) in [
        (
            ExportOutcome::Committed,
            RecoveryDisposition::Confirmed(
                batch
                    .records()
                    .iter()
                    .map(|record| record.metadata().identity.clone())
                    .collect(),
            ),
        ),
        (
            ExportOutcome::Retryable {
                error: error.clone(),
            },
            RecoveryDisposition::Retry,
        ),
        (
            ExportOutcome::Permanent {
                error: error.clone(),
            },
            RecoveryDisposition::Quarantine,
        ),
        (
            ExportOutcome::Uncertain {
                stage: ExportUncertainStage::CommitOutcomeUnknown,
                error,
            },
            RecoveryDisposition::Reconcile,
        ),
    ] {
        let report = ExportReport::for_batch(&batch, outcome).expect("report");
        assert_eq!(ledger.apply(&report), expected);
    }

    let partial = ExportOutcome::Partial {
        records: batch
            .records()
            .iter()
            .enumerate()
            .map(|(index, record)| ExportRecordOutcome {
                identity: record.metadata().identity.clone(),
                result: if index == 0 {
                    ExportRecordOutcomeKind::Committed
                } else {
                    ExportRecordOutcomeKind::Retryable {
                        error: ExportErrorCode::new("provider.record_retry")
                            .expect("partial error code"),
                    }
                },
            })
            .collect(),
    };
    let report = ExportReport::for_batch(&batch, partial).expect("partial report");
    assert_eq!(
        ledger.apply(&report),
        RecoveryDisposition::Partial(vec![batch.records()[0].metadata().identity.clone()])
    );
}

struct OfflineFactory {
    network_attempts: Arc<AtomicUsize>,
}

impl DatabaseProviderFactory for OfflineFactory {
    fn kind(&self) -> &'static str {
        "test.archive"
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
        assert_eq!(self.network_attempts.load(Ordering::SeqCst), 0);
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
        Ok(provider(Arc::new(ProviderState::default()), []))
    }
}

#[test]
fn validation_is_network_free_and_rejected_secrets_are_not_retained_in_errors() {
    let network_attempts = Arc::new(AtomicUsize::new(0));
    let factory = OfflineFactory {
        network_attempts: Arc::clone(&network_attempts),
    };
    let configuration = DatabaseConfiguration::new(
        uob_contracts::ExportDestinationId::new("analytics").expect("destination"),
        7,
    )
    .with_setting("credentials", ConfigurationValue::Text(SECRET.to_owned()));
    let Err(error) = factory.validate(&configuration) else {
        panic!("raw secret value must be rejected");
    };
    let rendered = format!("{error:?} {error}");

    assert_eq!(network_attempts.load(Ordering::SeqCst), 0);
    assert!(!rendered.contains(SECRET));
    assert_eq!(error.code(), ConfigurationErrorCode::InvalidField);
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(1), future)
        .await
        .expect("operation completed without hanging")
}

use std::future::Future;
