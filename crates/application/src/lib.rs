#![doc = "Target-neutral application coordination for Universal OCPP Bridge."]

mod storage;
mod target;

pub use storage::{
    AtomicStoreWrite, AtomicWriteOutcome, AuthorizationChange, AuthorizationReference,
    AuthorizationState, CommandAdmissionOutcome, CommittedRecord, CommittedRecordCursor,
    CommittedRecordId, CommittedRecordQuery, DeliveryId, Durability, OperationalStore, Page,
    PageLimit, PageLimitError, PendingDelivery, RecoveryBatch, RecoveryQuery, RetainedEventCursor,
    RetainedEventQuery, SnapshotCursor, SnapshotQuery, StorageError, StorageErrorCode,
    StorageFuture,
};
pub use target::{
    AcknowledgementScope, BridgeTarget, BridgeTargetFactory, ConfigurationError,
    ConfigurationErrorCode, ConfigurationField, ConfigurationFieldKind, ConfigurationSchema,
    ConfigurationValue, CredentialReference, DeliveryOutcome, DeliveryReport, DeliverySemantic,
    DiagnosticDrop, ErrorRetryClassification, TargetCapability, TargetCommandPort,
    TargetConfiguration, TargetContext, TargetDelivery, TargetDeliveryClass,
    TargetDeliveryReceiver, TargetDescriptor, TargetDiagnostic, TargetDiagnosticPort, TargetError,
    TargetErrorCode, TargetHealth, TargetHealthState, TargetLimits, TargetMessage,
    TargetMessageClass, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryPort, TargetQueryResult, TargetReportPort, TargetRuntimeLimits, TargetShutdown,
    TargetTask, ValidatedTargetConfiguration,
};

use uob_contracts::ContractVersion;

/// Composition-independent application facade.
#[derive(Clone, Debug, Default)]
pub struct Application;

impl Application {
    /// Creates the application facade without selecting any concrete adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Reports the contract version supported by the domain layer.
    #[must_use]
    pub const fn contract_version(&self) -> ContractVersion {
        uob_domain::supported_contract()
    }
}

#[cfg(test)]
mod tests {
    use super::Application;

    #[test]
    fn application_does_not_require_an_adapter_to_construct() {
        assert_eq!(Application::new().contract_version().major, 1);
    }
}
