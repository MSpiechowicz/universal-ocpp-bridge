#![doc = "Dependency-light shared contracts for Universal OCPP Bridge."]

mod command;
mod event;
mod export;
mod identity;
mod point;
mod snapshot;
mod timestamp;
mod trace;

pub use command::{
    AuthenticatedCommandOrigin, ChargingLimit, Command, CommandError, CommandErrorCode,
    CommandIdentityError, CommandLifecycle, CommandOperation, CommandRequest, CommandResult,
    CommandReturnRoute, CommandValidationError, ExternalCommand, ObservedCommandEffect,
    PayloadSchemaId, PrincipalId, PrivilegedOcppOperation, ProtocolActionName, RequestId,
    TargetInstanceId,
};
pub use event::{
    CorrelationId, EventEnvelope, EventId, EventIdentityError, EventOrigin, EventProvenance,
    EventType, ReplayError,
};
pub use export::{
    ExportBatch, ExportBatchError, ExportBatchId, ExportDestination, ExportDestinationId,
    ExportErrorCode, ExportIdentityError, ExportOutcome, ExportPayload, ExportPointChange,
    ExportRecord, ExportRecordId, ExportRecordIdentity, ExportRecordKind, ExportRecordMetadata,
    ExportRecordOutcome, ExportRecordOutcomeKind, ExportReport, ExportReportError,
    ExportResourceStatusChange, ExportSubrecordId, ExportUncertainStage,
};
pub use identity::{
    ArtifactDigest, BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource,
    Environment, IdentityError, NativeProtocolReference, ProcessInstanceId, ReleaseId, ResourceRef,
    RuntimeIdentity, StationId,
};
pub use point::{
    AccessMode, DataPointConstraints, DataPointDescriptor, DataPointValue, EngineeringUnit,
    ExactDecimal, ExactDecimalError, Freshness, MeasurementContext, MeasurementLocation,
    MeasurementMetadata, MeasurementPhase, NamedEnumValue, PointId, PointIdentityError, Quality,
    QualityLevel, SemanticName, TypedValue, UnitConversionError, ValueType,
};
pub use snapshot::{
    AvailabilityState, CapabilityError, CapabilityName, ChargingResourceSnapshot, Connectivity,
    Operation, OperationParameter, OptionalCapability, ParameterName, ProtocolCapabilityDetail,
    ProtocolEdition, ResourceCapabilities, StationSnapshot, SupportedOperation, TransactionId,
    TransactionSnapshot, TransactionState,
};
pub use timestamp::UtcTimestamp;
pub use trace::{
    RedactedTraceDetails, TargetKind, TraceDirection, TraceId, TraceIdentityError, TraceOutcome,
    TraceRecord, TraceSequence, TraceStage, TraceTarget,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Identifies the version of an application-owned contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ContractVersion {
    /// Major compatibility version.
    pub major: u16,
    /// Additive revision within the major version.
    pub revision: u16,
}

impl ContractVersion {
    /// The initial contract marker used while the canonical schemas are introduced.
    pub const V1_INITIAL: Self = Self {
        major: 1,
        revision: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::ContractVersion;

    #[test]
    fn initial_contract_has_expected_compatibility_major() {
        assert_eq!(ContractVersion::V1_INITIAL.major, 1);
    }
}
