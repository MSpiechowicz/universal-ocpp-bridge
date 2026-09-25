//! Sanitized OCPP 1.6 configuration evidence. Values are disclosed only for classified safe keys.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::RequestId;

/// Stable schema identity of the non-secret, durable configuration-write envelope.
pub const CONFIGURATION_CHANGE_REFERENCE_SCHEMA: &str =
    "urn:uob:ocpp16:ChangeConfigurationReference:1";

/// Bridge-owned schema for a protected write; the native OCA request is reconstructed
/// only after station/key-bound reference resolution at the authenticated socket.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConfigurationChangeReference {
    pub key: String,
    pub value_reference: String,
}

impl ConfigurationChangeReference {
    /// Checks the OCA key bound and opaque 256-bit capability shape without allocating.
    #[must_use]
    pub fn valid_parts(key: &str, reference: &str) -> bool {
        key.chars().count() <= 50
            && reference.len() == 68
            && reference.starts_with("cfg:")
            && reference.as_bytes()[4..].iter().all(u8::is_ascii_hexdigit)
    }
}

/// A returned key, including its independent read-only and value-presence facts.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationKey {
    pub key: String,
    pub readonly: bool,
    /// `None` means the charger omitted the value, not that it returned an empty string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// True when a value was returned but its classification forbids disclosure.
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted: bool,
}
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires &bool"
)]
fn is_false(value: &bool) -> bool {
    !value
}

/// Validated, bounded reply to one `GetConfiguration` or `ChangeConfiguration` call.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigurationResult {
    Read {
        /// Requested keys; absent means all keys and an empty list means an explicit empty list.
        #[serde(skip_serializing_if = "Option::is_none")]
        requested_keys: Option<Vec<String>>,
        /// Omitted when the charger omitted configurationKey; an empty list is explicit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keys: Option<Vec<ConfigurationKey>>,
        /// Omitted when the charger omitted unknownKey; an empty list is explicit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unknown_keys: Option<Vec<String>>,
    },
    Write {
        key: String,
        /// Exact OCPP status, including `RebootRequired` and `NotSupported`.
        status: ConfigurationWriteStatus,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ConfigurationWriteStatus {
    Accepted,
    Rejected,
    RebootRequired,
    NotSupported,
}

/// A separately requested later read, never inferred from protocol acknowledgement.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationObservation {
    pub read_request_id: RequestId,
    pub key: ConfigurationKey,
}
