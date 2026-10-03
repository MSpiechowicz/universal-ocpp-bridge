//! Reference-only OCPP 2.0.1 writes and value-free native acknowledgements.
use crate::{DeviceAttributeType201, DeviceComponent201, DeviceVariable201};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SET_VARIABLES_REFERENCE_SCHEMA_201: &str = "urn:uob:ocpp201:SetVariablesReference:1";
pub const SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201: &str =
    "urn:uob:ocpp201:SetNetworkProfileReference:1";
pub const CONFIGURATION_ITEMS_LIMIT_201: usize = 4096;
pub const CONFIGURATION_BYTES_LIMIT_201: usize = 256 * 1024;

/// Independently generated 256-bit capability; never a digest of secret content.
#[must_use]
pub fn valid_configuration_reference_201(reference: &str) -> bool {
    reference.len() == 71
        && reference.starts_with("cfg201:")
        && reference.as_bytes()[7..].iter().all(u8::is_ascii_hexdigit)
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetVariableReference201 {
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute_type: Option<DeviceAttributeType201>,
    pub value_reference: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetVariablesReference201 {
    pub set_variable_data: Vec<SetVariableReference201>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetNetworkProfileReference201 {
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub configuration_slot: i32,
    pub profile_reference: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SetVariableStatus201 {
    Accepted,
    Rejected,
    UnknownComponent,
    UnknownVariable,
    NotSupportedAttributeType,
    RebootRequired,
}
impl SetVariableStatus201 {
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::RebootRequired)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SetNetworkProfileStatus201 {
    Accepted,
    Rejected,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetVariableResult201 {
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
    pub attribute_type: DeviceAttributeType201,
    pub status: SetVariableStatus201,
}

/// No raw values, profiles, native extension data or reusable capabilities.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum ConfigurationResult201 {
    SetVariables {
        variables: Vec<SetVariableResult201>,
    },
    SetNetworkProfile {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        configuration_slot: i32,
        status: SetNetworkProfileStatus201,
        /// Native Accepted acknowledges staging only, never active connectivity.
        staged: bool,
    },
}
impl ConfigurationResult201 {
    #[must_use]
    pub fn accepted(&self) -> bool {
        match self {
            Self::SetVariables { variables } => {
                !variables.is_empty() && variables.iter().all(|item| item.status.accepted())
            }
            Self::SetNetworkProfile { status, .. } => {
                *status == SetNetworkProfileStatus201::Accepted
            }
        }
    }
}
