#![doc = "Target-neutral application coordination for Universal OCPP Bridge."]

mod access;
mod admission;
mod authorization;
mod command;
mod database;
mod diagnostic;
mod health;
mod payment;
mod protocol;
mod query;
mod resource;
mod security;
mod station;
mod storage;
mod target;

pub use access::{
    AccessGrant, AccessPermission, AccessPolicy, AccessPolicyError, AccessResourceScope,
    ScopedCommandAdmissionPort,
};
pub use admission::{
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
};
pub use authorization::{
    AuthorizationDecision, AuthorizationDenialReason, AuthorizationGuardedCommandPort,
    AuthorizationPolicyError, AuthorizationProvider, AuthorizationProviderDescriptor,
    AuthorizationProviderError, AuthorizationProviderFuture, LocalAuthorizationPolicy,
    LocalAuthorizationService, SensitiveAuthorizationToken,
};
pub use command::{
    CommandClock, CommandCoordinator, CommandDispatchOutcome, CommandRecoveryBatch,
    RecoveredCommand, StationCommandContext, StationCommandError, StationCommandFuture,
    StationCommandPort,
};
pub use database::{
    DatabaseAcknowledgementScope, DatabaseBatchReceiver, DatabaseConfiguration, DatabaseDiagnostic,
    DatabaseDiagnosticDrop, DatabaseDiagnosticPort, DatabaseError, DatabaseErrorCode,
    DatabaseExportContext, DatabaseHealth, DatabaseHealthState, DatabasePortError,
    DatabasePortErrorCode, DatabasePortFuture, DatabaseProvider, DatabaseProviderDescriptor,
    DatabaseProviderFactory, DatabaseProviderKind, DatabaseProviderLimits, DatabaseReportPort,
    DatabaseRetryClassification, DatabaseRuntimeLimits, DatabaseShutdown, DatabaseTask,
    DeduplicationCapability, TransactionCapability, ValidatedDatabaseConfiguration,
};
pub use diagnostic::{
    DiagnosticAttribute, DiagnosticBoundary, DiagnosticDisclosureAudit, DiagnosticObservation,
    DiagnosticOutcome, DiagnosticSerializationError, DiagnosticSummary, DiagnosticTraceContext,
    SafeDiagnosticField, SafeEndpointLabel, SafeEndpointLabelError, SanitizedDiagnostic,
    SensitiveDataClass, SensitiveDiagnosticValue, UnknownVendorPayload,
};
pub use health::{
    AuxiliaryProcess, ComponentHealth, ComponentHealthState, ComponentKind, CoreLoopState,
    HealthMonitor, HealthSnapshot, LatencyClass, LatencySnapshot, ProcessResourceMetrics,
    ReadinessState, StorageHealthState, StorageRetentionStatusView,
};
pub use payment::{
    CheckoutIntent, CheckoutIntentId, CheckoutPresentation, CheckoutRequest, PaymentAuditPort,
    PaymentAuthorizationAudit, PaymentAuthorizationInput, PaymentError, PaymentErrorCode,
    PaymentFuture, PaymentIntentStore, PaymentOrchestrator, PaymentProvider, PaymentProviderEvent,
    PaymentProviderId, PaymentVerificationReference, SensitivePaymentData, VerifiedPaymentEvent,
};
pub use protocol::{
    ChargerObservation, MeasurementApplyError, MeasurementObservation, RegistrationObservation,
    TransactionApplyError, TransactionApplyOutcome, TransactionEventKind,
    TransactionEventObservation, TransactionStartObservation, apply_measurements,
    apply_transaction_event,
};
pub use query::{
    CanonicalQuerySource, ScopedTargetQueryPort, TargetQueryAuthorization, TargetQueryPermission,
    TargetResourceScope,
};
pub use resource::{
    AdmissionError, AdmissionLimit, DEFAULT_AGGREGATE_QUEUED_PAYLOAD_BYTES,
    DEFAULT_MAX_CONNECTED_STATIONS, DEFAULT_MAX_OCPP_MESSAGE_BYTES, DEFAULT_TRACE_RING_BYTES,
    DiagnosticDropReason, LaggingConsumer, LaggingConsumerAction, ReplaceableTelemetrySlot,
    RuntimeQueueLimits, RuntimeQueueSnapshot, RuntimeReservation, RuntimeResourceBudget,
    RuntimeResourceLimits, RuntimeResourceSnapshot, StationAdmission, TelemetryReplaceOutcome,
    WorkClass,
};
pub use security::{IsolatedControl, RuntimeSecurityPolicy, SecurityPolicyError};
pub use station::{SizedStationOutput, StationEffects, StationInput, StationStateMachine};
pub use storage::{
    AtomicStoreWrite, AtomicWriteOutcome, AuthorizationChange, AuthorizationReference,
    AuthorizationState, COMMAND_DEDUPLICATION_RETENTION_SECONDS, CommandAdmissionOutcome,
    CommittedRecord, CommittedRecordCursor, CommittedRecordId, CommittedRecordQuery,
    DEFAULT_ACTIVE_SESSION_RESERVE_BYTES, DEFAULT_OPERATIONAL_STORAGE_BUDGET_BYTES,
    DeliveryAttempt, DeliveryAttemptResolution, DeliveryId, Durability,
    OPERATIONAL_HISTORY_RETENTION_SECONDS, OperationalStore, Page, PageLimit, PageLimitError,
    PendingDelivery, PendingDeliveryQuery, RETAINED_EVENT_CURSOR_PREFIX, RecordedDeliveryAttempt,
    RecoveryBatch, RecoveryQuery, RetainedEventCursor, RetainedEventPage, RetainedEventQuery,
    ScheduledDelivery, SnapshotCursor, SnapshotQuery, StorageAdmissionState, StorageError,
    StorageErrorCode, StorageFuture, StorageRetentionStatus, StorageWritePurpose,
    TargetDeliveryStore,
};
pub use target::{
    AcknowledgementScope, BridgeTarget, BridgeTargetFactory, ConfigurationError,
    ConfigurationErrorCode, ConfigurationField, ConfigurationFieldKind, ConfigurationSchema,
    ConfigurationValue, CredentialReference, DeliveryOutcome, DeliveryReport, DeliverySemantic,
    DiagnosticDrop, ErrorRetryClassification, RetainedEventItem, TargetCapability,
    TargetConfiguration, TargetContext, TargetDelivery, TargetDeliveryClass,
    TargetDeliveryReceiver, TargetDescriptor, TargetDiagnostic, TargetDiagnosticPort, TargetError,
    TargetErrorCode, TargetHealth, TargetHealthState, TargetLimits, TargetMessage,
    TargetMessageClass, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryPort, TargetQueryResult, TargetReportPort, TargetRetainedEventStream,
    TargetRuntimeLimits, TargetShutdown, TargetSubscription, TargetTask,
    ValidatedTargetConfiguration,
};

use uob_contracts::{ContractVersion, RuntimeIdentity, ServiceIdentity};

/// Composition-independent application facade.
#[derive(Clone, Debug)]
pub struct Application {
    identity: ServiceIdentity,
    health: HealthMonitor,
}

impl Application {
    /// Creates the application facade with composition-root-owned identity.
    ///
    /// # Panics
    ///
    /// Panics only if the compile-time default runtime limits become internally invalid.
    #[must_use]
    pub fn new(identity: ServiceIdentity) -> Self {
        Self::with_resource_limits(identity, RuntimeResourceLimits::default())
            .expect("default runtime resource limits are valid")
    }

    /// Creates the facade with one shared daemon resource budget and health monitor.
    ///
    /// # Errors
    ///
    /// Returns an admission error when any configured resource bound is invalid.
    pub fn with_resource_limits(
        identity: ServiceIdentity,
        limits: RuntimeResourceLimits,
    ) -> Result<Self, AdmissionError> {
        let budget = RuntimeResourceBudget::new(limits)?;
        Ok(Self {
            identity,
            health: HealthMonitor::new(budget),
        })
    }

    /// Reports the contract version supported by the domain layer.
    #[must_use]
    pub const fn contract_version(&self) -> ContractVersion {
        uob_domain::supported_contract()
    }

    /// Returns the trusted identity exposed to APIs and diagnostics.
    #[must_use]
    pub const fn identity(&self) -> &ServiceIdentity {
        &self.identity
    }

    /// Returns the canonical runtime context to attach to events and exports.
    #[must_use]
    pub const fn runtime_identity(&self) -> &RuntimeIdentity {
        &self.identity.runtime
    }

    /// Returns the shared health, readiness, and daemon resource authority.
    #[must_use]
    pub const fn health(&self) -> &HealthMonitor {
        &self.health
    }

    /// Returns security gates derived from the trusted runtime environment.
    #[must_use]
    pub const fn security_policy(&self) -> RuntimeSecurityPolicy {
        RuntimeSecurityPolicy::new(self.identity.runtime.environment)
    }
}

#[cfg(test)]
mod tests {
    use uob_contracts::{
        ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
        ServiceIdentity,
    };

    use super::Application;

    fn identity() -> ServiceIdentity {
        ServiceIdentity {
            bridge_id: BridgeId::new("bridge-test").expect("bridge ID"),
            runtime: RuntimeIdentity {
                environment: Environment::Demo,
                release_id: ReleaseId::new("test-release").expect("release ID"),
                release_digest: ArtifactDigest::new("sha256:test").expect("release digest"),
                process_instance_id: ProcessInstanceId::new("process-test").expect("process ID"),
            },
            selected_target_id: None,
        }
    }

    #[test]
    fn application_does_not_require_an_adapter_to_construct() {
        let identity = identity();
        let application = Application::new(identity.clone());

        assert_eq!(application.contract_version().major, 1);
        assert_eq!(application.identity(), &identity);
        assert_eq!(application.runtime_identity(), &identity.runtime);
    }
}
