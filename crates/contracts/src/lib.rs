#![doc = "Dependency-light shared contracts for Universal OCPP Bridge."]

mod charging_profile16;
mod charging_profile201;
pub use charging_profile201::*;
mod command;
pub use charging_profile16::*;
mod composite_schedule16;
mod configuration;
mod device_model201;
pub use device_model201::*;
mod event;
mod export;
mod identity;
mod point;
mod snapshot;
mod timestamp;
mod transaction16;
mod trigger;
mod trigger201;
pub use transaction16::Ocpp16TransactionEvidence;
mod trace;

pub use command::{
    AuthenticatedCommandOrigin, ChargingLimit, Command, CommandError, CommandErrorCode,
    CommandIdentityError, CommandLifecycle, CommandOperation, CommandOperationKind, CommandRequest,
    CommandResult, CommandReturnRoute, CommandSummary, CommandValidationError, ExternalCommand,
    ObservedCommandEffect, PayloadSchemaId, PrincipalId, PrivilegedOcppOperation,
    ProtocolActionName, RequestId, TargetInstanceId,
};
pub use composite_schedule16::{
    CompositeSchedule16, CompositeSchedulePeriod16, CompositeScheduleRateUnit16,
    CompositeScheduleRequest16, CompositeScheduleResult16, CompositeScheduleStatus16,
};
pub use configuration::{
    CONFIGURATION_CHANGE_REFERENCE_SCHEMA, ConfigurationChangeReference, ConfigurationKey,
    ConfigurationObservation, ConfigurationResult, ConfigurationWriteStatus,
};
pub use event::{
    CorrelationId, EventEnvelope, EventId, EventIdentityError, EventOrigin, EventProvenance,
    EventType, ReplayError, StationEvent,
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
    RuntimeIdentity, ServiceIdentity, StationId,
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
    TransactionProtocolState, TransactionSnapshot, TransactionState,
};
pub use timestamp::UtcTimestamp;
pub use trace::{
    RedactedTraceDetails, TargetKind, TraceDirection, TraceId, TraceIdentityError, TraceOutcome,
    TraceRecord, TraceSequence, TraceStage, TraceTarget,
};
pub use trigger::{
    TriggerMessageClass, TriggerNativeResponse, TriggerObservation, TriggerObservationStatus,
    TriggerObservedEvent,
};
pub use trigger201::{
    TriggerEvse201, TriggerMessageClass201, TriggerNativeResponse201, TriggerNativeStatus201,
    TriggerObservation201, TriggerObservationStatus201, TriggerObservedEvent201,
    TriggerStatusInfo201, TriggerTarget201,
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
    /// Additive configuration-result revision of the command result contract.
    pub const V1_CONFIGURATION: Self = Self {
        major: 1,
        revision: 1,
    };
    /// Additive OCPP 1.6 trigger observation revision.
    pub const V1_TRIGGER: Self = Self {
        major: 1,
        revision: 2,
    };
    /// Additive OCPP 2.0.1 trigger observation revision.
    pub const V1_TRIGGER_201: Self = Self {
        major: 1,
        revision: 3,
    };
    /// Additive OCPP 1.6 composite-schedule evidence revision.
    pub const V1_COMPOSITE_SCHEDULE_16: Self = Self {
        major: 1,
        revision: 4,
    };
    /// Additive read-only OCPP 2.0.1 device-model evidence revision.
    pub const V1_DEVICE_MODEL_201: Self = Self {
        major: 1,
        revision: 5,
    };
    /// Additive full native OCPP 1.6 profile acknowledgement evidence.
    pub const V1_CHARGING_PROFILE_16: Self = Self {
        major: 1,
        revision: 6,
    };
    /// Additive full native OCPP 2.0.1 profile acknowledgement evidence.
    pub const V1_CHARGING_PROFILE_201: Self = Self {
        major: 1,
        revision: 7,
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

mod remote_control;
pub use remote_control::RemoteControlEvidence;
