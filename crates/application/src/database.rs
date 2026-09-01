use std::{
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use uob_contracts::{
    ContractVersion, ExportBatch, ExportDestinationId, ExportOutcome, ExportRecordKind,
    ExportReport, UtcTimestamp,
};

use crate::{ConfigurationError, ConfigurationSchema};

mod configuration;

pub use configuration::{DatabaseConfiguration, ValidatedDatabaseConfiguration};

/// Future returned by a host-owned database-export port.
pub type DatabasePortFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), DatabasePortError>> + Send + 'a>>;

/// One boxed, supervised external database-provider session.
pub type DatabaseTask = Pin<Box<dyn Future<Output = Result<(), DatabaseError>> + Send + 'static>>;

/// Object-safe lifecycle implemented by every one-way external database adapter.
pub trait DatabaseProvider: Send + 'static {
    /// Describes this configured provider and its exact delivery guarantees.
    fn descriptor(&self) -> DatabaseProviderDescriptor;

    /// Starts the long-lived provider task without granting application or storage access.
    fn run(self: Box<Self>, context: DatabaseExportContext) -> DatabaseTask;
}

/// Object-safe, network-free validation and construction boundary for a provider kind.
pub trait DatabaseProviderFactory: Send + Sync {
    /// Stable registered provider kind.
    fn kind(&self) -> &'static str;

    /// Safe configuration shape. Credential values are never part of the schema.
    fn configuration_schema(&self) -> ConfigurationSchema;

    /// Validates settings without resolving credentials, DNS, or database connections.
    ///
    /// # Errors
    ///
    /// Returns a sanitized configuration error without retaining a rejected value.
    fn validate(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<ValidatedDatabaseConfiguration, ConfigurationError>;

    /// Constructs an inactive provider without opening a connection.
    ///
    /// # Errors
    ///
    /// Returns a sanitized configuration error when construction is not possible.
    fn create(
        &self,
        configuration: ValidatedDatabaseConfiguration,
    ) -> Result<Box<dyn DatabaseProvider>, ConfigurationError>;
}

/// Stable registered kind for one external database adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseProviderKind(String);

impl DatabaseProviderKind {
    /// Creates a non-empty stable provider kind.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseErrorCode::InvalidConfiguration`] for empty text.
    pub fn new(value: impl Into<String>) -> Result<Self, DatabaseError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DatabaseError::permanent(
                DatabaseErrorCode::InvalidConfiguration,
                "provider.kind.empty",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the stable provider kind.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How an adapter detects repeated at-least-once records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeduplicationCapability {
    /// The destination cannot detect duplicate canonical identities.
    None,
    /// Stable record and subrecord identities are checked before commit.
    StableRecordIdentity,
}

/// Commit boundary an adapter can prove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionCapability {
    /// Every record in a batch commits atomically.
    AtomicBatch,
    /// Records commit independently and require complete per-record outcomes.
    PerRecord,
}

/// Exact fact represented by a successful provider acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatabaseAcknowledgementScope {
    /// The destination confirmed an atomic commit of the complete batch.
    AtomicRemoteCommit,
    /// The destination confirms commit separately for each record.
    PerRecordRemoteCommit,
    /// Only transport handoff is known; it is not checkpoint evidence.
    TransportHandoff(String),
}

/// Static host-enforced limits advertised by a provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseProviderLimits {
    /// Largest number of records accepted in one batch.
    pub maximum_records_per_batch: usize,
    /// Largest encoded canonical batch accepted by the mapping.
    pub maximum_batch_bytes: usize,
    /// Largest number of batches processed concurrently.
    pub maximum_in_flight_batches: usize,
}

/// Complete supported surface of one configured database provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseProviderDescriptor {
    /// Stable registered adapter kind.
    pub kind: DatabaseProviderKind,
    /// Stable configured destination identity.
    pub instance_id: ExportDestinationId,
    /// Canonical export schema versions accepted by the adapter.
    pub record_schema_versions: Vec<ContractVersion>,
    /// Explicitly supported canonical record classes.
    pub supported_record_classes: Vec<ExportRecordKind>,
    /// Payload and concurrency bounds.
    pub limits: DatabaseProviderLimits,
    /// Duplicate-detection guarantee.
    pub deduplication: DeduplicationCapability,
    /// Remote transaction boundary.
    pub transactions: TransactionCapability,
    /// Exact meaning of provider acknowledgement.
    pub acknowledgement_scope: DatabaseAcknowledgementScope,
}

impl DatabaseProviderDescriptor {
    /// Rejects a batch outside the descriptor's destination, schema, class, or count surface.
    ///
    /// # Errors
    ///
    /// Returns a stable sanitized provider error for the first unsupported property.
    pub fn validate_batch(&self, batch: &ExportBatch) -> Result<(), DatabaseError> {
        if batch.destination().destination_id != self.instance_id {
            return Err(DatabaseError::permanent(
                DatabaseErrorCode::InvalidBatch,
                "batch.destination_mismatch",
            ));
        }
        if batch.records().len() > self.limits.maximum_records_per_batch {
            return Err(DatabaseError::retryable(
                DatabaseErrorCode::CapacityExhausted,
                "batch.record_limit",
            ));
        }
        for record in batch.records() {
            if !self
                .record_schema_versions
                .contains(&record.metadata().schema_version)
            {
                return Err(DatabaseError::permanent(
                    DatabaseErrorCode::UnsupportedSchemaVersion,
                    "batch.schema_version",
                ));
            }
            if !self.supported_record_classes.contains(&record.kind()) {
                return Err(DatabaseError::permanent(
                    DatabaseErrorCode::UnsupportedRecordClass,
                    "batch.record_class",
                ));
            }
        }
        Ok(())
    }

    /// Ensures a report does not claim a stronger transaction boundary than advertised.
    ///
    /// # Errors
    ///
    /// Atomic providers cannot report partial commit and per-record providers cannot report an
    /// atomic batch commit.
    pub fn validate_report(&self, report: &ExportReport) -> Result<(), DatabaseError> {
        let incompatible = matches!(
            (self.transactions, report.outcome()),
            (
                TransactionCapability::AtomicBatch,
                ExportOutcome::Partial { .. }
            ) | (TransactionCapability::PerRecord, ExportOutcome::Committed)
        );
        if incompatible {
            return Err(DatabaseError::permanent(
                DatabaseErrorCode::InvalidReport,
                "report.transaction_scope",
            ));
        }
        Ok(())
    }
}

/// Stable provider session failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseErrorCode {
    /// Supplied runtime context is inconsistent with the descriptor.
    InvalidContext,
    /// Provider configuration cannot be used.
    InvalidConfiguration,
    /// A bounded capacity was exhausted.
    CapacityExhausted,
    /// The external database connection is unavailable.
    ConnectionUnavailable,
    /// A record class was not advertised by the provider.
    UnsupportedRecordClass,
    /// A canonical record schema version is unsupported.
    UnsupportedSchemaVersion,
    /// A batch is malformed or bound to another destination.
    InvalidBatch,
    /// A report overstates or contradicts provider guarantees.
    InvalidReport,
    /// Graceful shutdown exceeded its deadline.
    ShutdownDeadlineExceeded,
}

/// Whether supervision may retry a provider failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseRetryClassification {
    /// Retry can be attempted under bounded host backoff.
    Retryable,
    /// Operator or configuration intervention is required.
    Permanent,
    /// Remote side effects must be reconciled before retry.
    Uncertain,
}

/// Structured sanitized provider failure without driver source strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseError {
    code: DatabaseErrorCode,
    retry: DatabaseRetryClassification,
    context: String,
}

impl DatabaseError {
    fn retryable(code: DatabaseErrorCode, context: impl Into<String>) -> Self {
        Self::new(code, DatabaseRetryClassification::Retryable, context)
    }

    fn permanent(code: DatabaseErrorCode, context: impl Into<String>) -> Self {
        Self::new(code, DatabaseRetryClassification::Permanent, context)
    }

    /// Creates an error from stable, pre-sanitized fields.
    #[must_use]
    pub fn new(
        code: DatabaseErrorCode,
        retry: DatabaseRetryClassification,
        context: impl Into<String>,
    ) -> Self {
        Self {
            code,
            retry,
            context: context.into(),
        }
    }

    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> DatabaseErrorCode {
        self.code
    }

    /// Returns restart/reconciliation classification.
    #[must_use]
    pub const fn retry_classification(&self) -> DatabaseRetryClassification {
        self.retry
    }

    /// Returns bounded sanitized context, never a connection string or source error.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "database provider {:?} ({:?}): {}",
            self.code, self.retry, self.context
        )
    }
}

impl Error for DatabaseError {}

/// Poll-based bounded batch receiver with no storage or subscription surface.
pub trait DatabaseBatchReceiver: Send {
    /// Polls the next host-owned canonical batch.
    fn poll_receive(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<ExportBatch>>;

    /// Maximum number of batches that can be queued.
    fn capacity(&self) -> usize;

    /// Current queued batch count for bounded diagnostics.
    fn backlog(&self) -> usize;
}

/// Reliable report channel consumed by host-owned checkpoint and retry policy.
pub trait DatabaseReportPort: Send + Sync {
    /// Reports a validated exact export outcome with bounded backpressure.
    fn report(&self, report: ExportReport) -> DatabasePortFuture<'_>;
}

/// Stable host-port failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabasePortErrorCode {
    /// The bounded critical report channel cannot currently accept work.
    Busy,
    /// The host has closed the report channel.
    Closed,
    /// The report does not match scheduled work.
    InvalidReport,
}

/// Sanitized failure returned by the critical report port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabasePortError {
    code: DatabasePortErrorCode,
    context: String,
}

impl DatabasePortError {
    /// Creates an error from a stable sanitized context code.
    #[must_use]
    pub fn new(code: DatabasePortErrorCode, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(&self) -> DatabasePortErrorCode {
        self.code
    }

    /// Returns sanitized application context.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl fmt::Display for DatabasePortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "database report port {:?}: {}",
            self.code, self.context
        )
    }
}

impl Error for DatabasePortError {}

/// Runtime states visible to provider supervision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseHealthState {
    /// Provider construction completed and startup is in progress.
    Starting,
    /// Provider can currently perform its advertised work.
    Ready,
    /// Some supported work is impaired.
    Degraded,
    /// The provider is recovering its external connection.
    Reconnecting,
    /// The supervised provider has stopped.
    Stopped,
}

/// Sanitized bounded provider health report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseHealth {
    /// Current lifecycle state.
    pub state: DatabaseHealthState,
    /// Batches waiting in the bounded host receiver.
    pub batch_backlog: usize,
    /// Current in-flight batch attempts.
    pub in_flight_batches: usize,
    /// Current provider-owned database connections.
    pub active_connections: usize,
    /// Stable sanitized reason code for non-ready states.
    pub reason: Option<String>,
}

/// Low-priority provider diagnostics, separate from critical export reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatabaseDiagnostic {
    /// Structured provider lifecycle and resource state.
    Health(DatabaseHealth),
    /// Sanitized provider-specific event code and bounded context code.
    Event { code: String, context: String },
}

/// Explicit reason a best-effort diagnostic was not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseDiagnosticDrop {
    /// The bounded diagnostic channel is full.
    Full,
    /// Diagnostics are disabled for this provider.
    Disabled,
    /// The diagnostic channel is shutting down.
    Closed,
}

/// Best-effort diagnostics that cannot block critical export reports.
pub trait DatabaseDiagnosticPort: Send + Sync {
    /// Attempts to emit without waiting for capacity.
    ///
    /// # Errors
    ///
    /// Returns an explicit drop reason when the diagnostic is not accepted.
    fn try_emit(&self, diagnostic: DatabaseDiagnostic) -> Result<(), DatabaseDiagnosticDrop>;
}

/// Poll-based graceful-shutdown signal independent of an async runtime.
pub trait DatabaseShutdown: Send {
    /// Resolves when host supervision requests shutdown.
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()>;
}

/// Host-enforced resource limits for a running provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseRuntimeLimits {
    /// Maximum concurrent batch attempts.
    pub maximum_in_flight_batches: usize,
    /// Maximum records accepted from one batch.
    pub maximum_records_per_batch: usize,
    /// Maximum encoded canonical batch size.
    pub maximum_batch_bytes: usize,
}

/// Complete one-way host capability set supplied to a provider session.
///
/// The context deliberately contains no command/query port, charger socket, database handle,
/// arbitrary SQL surface, or unrestricted event subscription.
pub struct DatabaseExportContext {
    /// Bounded canonical export batches.
    pub batches: Pin<Box<dyn DatabaseBatchReceiver>>,
    /// Critical outcomes used by durable checkpoint and retry policy.
    pub critical_reports: Arc<dyn DatabaseReportPort>,
    /// Low-priority health and diagnostics.
    pub diagnostics: Arc<dyn DatabaseDiagnosticPort>,
    /// Host-enforced bounds.
    pub limits: DatabaseRuntimeLimits,
    /// Graceful shutdown signal.
    pub shutdown: Pin<Box<dyn DatabaseShutdown>>,
    /// Absolute deadline by which graceful shutdown must finish.
    pub shutdown_deadline: UtcTimestamp,
}

#[cfg(test)]
mod tests;
