use std::{error::Error, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::TargetInstanceId;

macro_rules! stable_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier after rejecting empty or whitespace-only values.
            ///
            /// # Errors
            ///
            /// Returns [`IdentityError::Empty`] when `value` has no visible characters.
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(IdentityError::Empty(stringify!($name)));
                }
                Ok(Self(value))
            }

            /// Returns the stable wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
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

stable_id!(
    BridgeId,
    "Stable logical identity of a bridge installation."
);
stable_id!(StationId, "Stable logical identity of a charging station.");
stable_id!(CanonicalEvseId, "Stable canonical identity of an EVSE.");
stable_id!(
    CanonicalConnectorId,
    "Stable canonical identity of a connector."
);
stable_id!(ReleaseId, "Immutable identity of a built release.");
stable_id!(
    ArtifactDigest,
    "Content digest identifying the immutable release artifact."
);
stable_id!(
    ProcessInstanceId,
    "Unique identity of one running process instance."
);

/// Failure to construct a trusted identity value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The named identity type was given an empty value.
    Empty(&'static str),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty(kind) => write!(formatter, "{kind} cannot be empty"),
        }
    }
}

impl Error for IdentityError {}

/// Trusted runtime environment assigned by the process composition root.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// Live charging environment.
    Production,
    /// Isolated qualification environment.
    Staging,
    /// Explicit local demonstration or test environment.
    Demo,
}

/// Immutable process and release context attached to emitted records.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RuntimeIdentity {
    /// Trusted environment of this process.
    pub environment: Environment,
    /// Immutable release identity.
    pub release_id: ReleaseId,
    /// Digest of the running release artifact.
    pub release_digest: ArtifactDigest,
    /// Unique identity of this process invocation.
    pub process_instance_id: ProcessInstanceId,
}

/// Trusted bridge, release, process, and selected-target context exposed by the service.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceIdentity {
    /// Stable logical bridge installation configured for this process.
    pub bridge_id: BridgeId,
    /// Canonical environment, release, and process context.
    pub runtime: RuntimeIdentity,
    /// Explicit selected target, absent for an API-only service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_target_id: Option<TargetInstanceId>,
}

/// Optional canonical charging resource below a station.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalResource {
    /// OCPP 1.6-style connector resource.
    Connector {
        /// Stable connector identity.
        connector_id: CanonicalConnectorId,
    },
    /// OCPP 2.0.1-style EVSE, optionally narrowed to one connector.
    Evse {
        /// Stable EVSE identity.
        evse_id: CanonicalEvseId,
        /// Stable connector identity within the EVSE, when applicable.
        #[serde(skip_serializing_if = "Option::is_none")]
        connector_id: Option<CanonicalConnectorId>,
    },
}

/// Original protocol-specific address retained as mapping evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum NativeProtocolReference {
    /// Native OCPP 1.6 connector number.
    Ocpp16 {
        /// Connector number from the OCPP frame.
        connector_id: u32,
    },
    /// Native OCPP 2.0.1 EVSE and optional connector numbers.
    Ocpp201 {
        /// EVSE number from the OCPP frame.
        evse_id: u32,
        /// Connector number within the EVSE, when supplied.
        #[serde(skip_serializing_if = "Option::is_none")]
        connector_id: Option<u32>,
    },
}

/// Stable canonical resource identity, independent of runtime and transports.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ResourceRef {
    /// Bridge installation that owns the resource.
    pub bridge_id: BridgeId,
    /// Charging station that owns the resource.
    pub station_id: StationId,
    /// Optional connector or EVSE below the station.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<CanonicalResource>,
    /// Optional protocol address retained separately from canonical identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_protocol_reference: Option<NativeProtocolReference>,
}
