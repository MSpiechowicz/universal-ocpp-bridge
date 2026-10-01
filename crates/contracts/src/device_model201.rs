//! Native OCPP 2.0.1 read-only device-model evidence. Values are disclosed fail-closed.
/// Maximum escaped JSON bytes retained for a native device-model command result.
pub const DEVICE_MODEL_OUTPUT_LIMIT_201: usize = 1024 * 1024;
use crate::{CorrelationId, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Native EVSE/connector scope, not canonical topology.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceEvse201 {
    pub id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<i32>,
}

/// Case-preserving native component identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceComponent201 {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evse: Option<DeviceEvse201>,
}

/// Case-preserving native variable identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceVariable201 {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

/// Native attribute type; omitted requests mean Actual.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DeviceAttributeType201 {
    #[default]
    Actual,
    Target,
    MinSet,
    MaxSet,
}

/// One exact `GetVariables` request identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceVariableQuery201 {
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute_type: Option<DeviceAttributeType201>,
}

/// Native selector: absent variable and instances remain wildcards.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSelector201 {
    pub component: DeviceComponent201,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<DeviceVariable201>,
}

/// Supported native report bases.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DeviceReportBase201 {
    ConfigurationInventory,
    FullInventory,
    SummaryInventory,
}

/// Native OR criteria, not host-side filters.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DeviceComponentCriterion201 {
    Active,
    Available,
    Enabled,
    Problem,
}

/// Immutable query and exact native report request ID.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action")]
pub enum DeviceModelQuery201 {
    GetVariables {
        entries: Vec<DeviceVariableQuery201>,
    },
    GetBaseReport {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        request_id: i32,
        report_base: DeviceReportBase201,
    },
    GetReport {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        request_id: i32,
        selectors: Vec<DeviceSelector201>,
        criteria: Vec<DeviceComponentCriterion201>,
    },
}
impl DeviceModelQuery201 {
    #[must_use]
    pub const fn request_id(&self) -> Option<i32> {
        match self {
            Self::GetVariables { .. } => None,
            Self::GetBaseReport { request_id, .. } | Self::GetReport { request_id, .. } => {
                Some(*request_id)
            }
        }
    }
}

/// Native independent per-variable outcome.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DeviceVariableStatus201 {
    Accepted,
    Rejected,
    UnknownComponent,
    UnknownVariable,
    NotSupportedAttributeType,
}

/// Disclosure-safe value, retaining absent versus explicitly empty versus redacted.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceValue201 {
    pub present: bool,
    pub redacted: bool,
    pub empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Matched native result; native names and instances are preserved.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceVariableResult201 {
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
    pub attribute_type: DeviceAttributeType201,
    pub status: DeviceVariableStatus201,
    pub value: DeviceValue201,
}

/// Exact native report acknowledgement, independent of collection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DeviceReportAck201 {
    Accepted,
    Rejected,
    NotSupported,
    EmptyResultSet,
}

/// Safe native attribute metadata.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceReportAttribute201 {
    pub attribute_type: DeviceAttributeType201,
    pub value: DeviceValue201,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutability: Option<DeviceMutability201>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constant: Option<bool>,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DeviceMutability201 {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

/// Specification-defined characteristics; opaque valuesList is deliberately excluded.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceCharacteristics201 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub data_type: DeviceDataType201,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_limit: Option<serde_json::Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_limit: Option<serde_json::Number>,
    pub supports_monitoring: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceDataType201 {
    String,
    Decimal,
    Integer,
    DateTime,
    Boolean,
    #[serde(rename = "OptionList")]
    OptionList,
    #[serde(rename = "SequenceList")]
    SequenceList,
    #[serde(rename = "MemberList")]
    MemberList,
}

/// Ordered report item, never used to update topology or grants.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceReportItem201 {
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
    pub attributes: Vec<DeviceReportAttribute201>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub characteristics: Option<DeviceCharacteristics201>,
}

/// Metadata for every accepted native fragment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceReportFragment201 {
    pub generated_at: String,
    pub sequence: u32,
    pub more: bool,
    pub items: usize,
}

/// Counts include only accepted fragments; recovered unavailable counts remain absent.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceReportProgress201 {
    pub fragments: u32,
    pub items: usize,
    pub bytes: usize,
}

/// Payload-free terminal reason.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceReportFailure201 {
    Timeout,
    ByteLimit,
    ItemLimit,
    FragmentLimit,
    Capacity,
    DuplicateSequence,
    OutOfOrderSequence,
    Correlation,
    InvalidFragment,
    Disconnected,
    Interrupted,
    OutputLimit,
    StorageUnavailable,
    NotTransmitted,
    NativeRejected,
    MissingAcknowledgement,
}

/// Collection result is independent of the native CALL response.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceReportState201 {
    Pending,
    Complete {
        progress: DeviceReportProgress201,
        fragments: Vec<DeviceReportFragment201>,
        items: Vec<DeviceReportItem201>,
    },
    Incomplete {
        reason: DeviceReportFailure201,
        progress: Option<DeviceReportProgress201>,
    },
    NotExpected,
}
impl DeviceReportState201 {
    #[must_use]
    pub const fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Typed disclosure-safe command evidence, with immutable connection context.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeviceModelResult201 {
    pub query: DeviceModelQuery201,
    pub connection: CorrelationId,
    pub generation: u64,
    pub dispatch_recorded_at: UtcTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_ack: Option<DeviceReportAck201>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<DeviceVariableResult201>,
    pub report: DeviceReportState201,
}
