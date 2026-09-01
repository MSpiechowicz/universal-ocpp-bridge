use std::{error::Error, fmt};

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{
    ContractVersion, DataPointConstraints, DataPointDescriptor, DataPointValue, ResourceRef,
    TypedValue, UtcTimestamp, ValueType,
};

macro_rules! snapshot_text {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a value after rejecting empty or whitespace-only text.
            ///
            /// # Errors
            ///
            /// Returns [`CapabilityError::EmptyIdentity`] when `value` has no visible text.
            pub fn new(value: impl Into<String>) -> Result<Self, CapabilityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(CapabilityError::EmptyIdentity(stringify!($name)));
                }
                Ok(Self(value))
            }

            /// Returns the stable wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

snapshot_text!(
    TransactionId,
    "Stable canonical identity of a charging transaction."
);
snapshot_text!(
    CapabilityName,
    "Stable name of an optional, explicitly advertised capability."
);
snapshot_text!(
    ParameterName,
    "Stable name of a parameter accepted by a supported operation."
);

/// Protocol edition negotiated with a station.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolEdition {
    /// OCPP 1.6J.
    Ocpp16j,
    /// OCPP 2.0.1.
    Ocpp201,
}

/// Current observed station connection state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Connectivity {
    /// No admitted station connection exists.
    Disconnected,
    /// A transport is being authenticated or negotiated.
    Connecting,
    /// The station has an admitted connection using the stated protocol edition.
    Connected {
        /// Negotiated protocol edition.
        protocol: ProtocolEdition,
        /// Time at which the current connection was admitted.
        connected_at: UtcTimestamp,
        /// Most recent observed station activity, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        last_message_at: Option<UtcTimestamp>,
    },
}

/// Current observed operational availability of a charging resource.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityState {
    /// Resource is available for use.
    Available,
    /// Resource is occupied by a charging workflow.
    Occupied,
    /// Resource is not available for operation.
    Unavailable,
    /// Resource reports a fault.
    Faulted,
    /// No reliable observation is currently available.
    Unknown,
}

/// Application-owned operation that a resource may explicitly advertise.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    /// Start a charging transaction.
    Start,
    /// Stop an identified charging transaction.
    Stop,
    /// Set a charging current or power limit.
    SetChargingLimit,
    /// Change operational availability.
    ChangeAvailability,
    /// Reset a station or charging resource.
    Reset,
    /// Unlock a connector.
    UnlockConnector,
    /// A schema-validated protocol-specific management action.
    ProtocolAction {
        /// Protocol edition that defines the action.
        protocol: ProtocolEdition,
        /// Exact protocol action name.
        action: String,
    },
}

/// Declared constraint for one operation parameter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationParameter {
    /// Stable parameter name.
    pub name: ParameterName,
    /// Primitive value type accepted by the parameter.
    pub value_type: ValueType,
    /// Whether callers must supply the parameter.
    pub required: bool,
    /// Optional bounds or named enumeration members.
    #[serde(default, skip_serializing_if = "DataPointConstraints::is_empty")]
    pub constraints: DataPointConstraints,
}

/// One explicitly supported operation and its accepted parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupportedOperation {
    /// Supported operation identity.
    pub operation: Operation,
    /// Complete declared parameter surface for the operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<OperationParameter>,
}

/// Optional capability that is independent of command support.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OptionalCapability {
    /// Stable capability name.
    pub name: CapabilityName,
    /// Optional typed capability value; absence means advertised presence only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<TypedValue>,
}

/// Version-specific capability fact retained without importing an OCPP adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolCapabilityDetail {
    /// Protocol edition whose semantics define this fact.
    pub protocol: ProtocolEdition,
    /// Stable protocol-defined or bridge-defined fact name.
    pub name: CapabilityName,
    /// Typed value of the advertised fact.
    pub value: TypedValue,
}

/// Explicit operation and optional-capability surface of one resource.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceCapabilities {
    /// Operations the charger is known to support for this resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<SupportedOperation>,
    /// Independent optional features advertised by the station.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<OptionalCapability>,
    /// Protocol-specific facts needed to preserve the advertised meaning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_details: Vec<ProtocolCapabilityDetail>,
}

impl ResourceCapabilities {
    /// Returns whether an exact operation is explicitly supported.
    #[must_use]
    pub fn supports(&self, operation: &Operation) -> bool {
        self.operations
            .iter()
            .any(|supported| &supported.operation == operation)
    }

    /// Returns the descriptor for an explicitly supported operation.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError::UnsupportedOperation`] when no exact matching
    /// operation was advertised. Empty capability sets therefore remain read-only.
    pub fn require(&self, operation: &Operation) -> Result<&SupportedOperation, CapabilityError> {
        self.operations
            .iter()
            .find(|supported| &supported.operation == operation)
            .ok_or_else(|| CapabilityError::UnsupportedOperation(operation.clone()))
    }
}

/// Failure to construct or query a capability descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    /// The named stable identity had no visible characters.
    EmptyIdentity(&'static str),
    /// The resource did not explicitly advertise the requested operation.
    UnsupportedOperation(Operation),
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentity(kind) => write!(formatter, "{kind} cannot be empty"),
            Self::UnsupportedOperation(operation) => {
                write!(formatter, "operation {operation:?} is not supported")
            }
        }
    }
}

impl Error for CapabilityError {}

/// Current observed lifecycle state of a charging transaction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    /// Transaction has been admitted but charging has not been observed.
    Pending,
    /// Charging activity is currently observed.
    Active,
    /// Transaction is temporarily suspended.
    Suspended,
    /// Transaction has ended.
    Ended,
    /// Final outcome cannot yet be reconciled.
    Uncertain,
}

/// Observed transaction state associated with a canonical charging resource.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionSnapshot {
    /// Stable transaction identity.
    pub transaction_id: TransactionId,
    /// Resource associated with the transaction.
    pub resource: ResourceRef,
    /// Current observed lifecycle state.
    pub state: TransactionState,
    /// Source-reported or bridge-observed start time.
    pub started_at: UtcTimestamp,
    /// Observed end time, when the transaction has ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UtcTimestamp>,
}

/// Observed state and advertised model of one connector or EVSE resource.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChargingResourceSnapshot {
    /// Canonical resource and separately retained native protocol address.
    pub resource: ResourceRef,
    /// Current observed availability.
    pub availability: AvailabilityState,
    /// Explicit resource-local operation and optional capability surface.
    pub capabilities: ResourceCapabilities,
    /// Descriptors for points owned by this resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_points: Vec<DataPointDescriptor>,
    /// Latest observed values for resource points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_values: Vec<DataPointValue>,
}

/// Transport-neutral observed station state and advertised capability model.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StationSnapshot {
    /// Version of this canonical snapshot contract.
    pub schema_version: ContractVersion,
    /// Station-level canonical reference.
    pub station: ResourceRef,
    /// UTC instant at which the bridge assembled this snapshot.
    pub observed_at: UtcTimestamp,
    /// Current observed connectivity.
    pub connectivity: Connectivity,
    /// Explicit operations and optional capabilities applying to the station.
    pub capabilities: ResourceCapabilities,
    /// Version-preserving connector or EVSE topology.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ChargingResourceSnapshot>,
    /// Current and recently ended transactions represented canonically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transactions: Vec<TransactionSnapshot>,
    /// Latest station-level point values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_values: Vec<DataPointValue>,
}
