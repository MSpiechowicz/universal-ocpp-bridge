//! OCPP 1.6 Security Whitepaper `SecurityEventNotification` evidence, journaled as an observed
//! station fact. Free-form `techInfo` is retained only after credential redaction; it never
//! changes configuration, policy or canonical state.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

/// Maximum characters of the native `type` (`SecurityEventNotification.req`, `maxLength` 50).
pub const SECURITY_EVENT_TYPE_MAX_CHARS_16: usize = 50;
/// Maximum characters retained from `techInfo`. The native limit is 255; a redaction marker can
/// be longer than the value it replaces, and the larger bound keeps such a record valid.
pub const SECURITY_EVENT_TECH_INFO_MAX_CHARS_16: usize = 1024;

fn bounded_text<'de, D: Deserializer<'de>, const MAX: usize>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.chars().count() > MAX {
        return Err(de::Error::custom(format!("text exceeds {MAX} characters")));
    }
    Ok(value)
}

fn bounded_optional_text<'de, D: Deserializer<'de>, const MAX: usize>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    if value
        .as_deref()
        .is_some_and(|text| text.chars().count() > MAX)
    {
        return Err(de::Error::custom(format!("text exceeds {MAX} characters")));
    }
    Ok(value)
}

// Serde passes a reference to `skip_serializing_if` predicates.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// One acknowledged `SecurityEventNotification.req`, journaled as observed evidence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvent16 {
    /// Native event `type`, kept verbatim: vendor-specific names are never rejected.
    #[serde(deserialize_with = "bounded_text::<_, SECURITY_EVENT_TYPE_MAX_CHARS_16>")]
    #[schemars(length(max = 50))]
    pub event_type: String,
    /// The type is one of the standard event names of the Security Whitepaper (section 8).
    pub standard: bool,
    /// Charger-reported time of the event, normalised to UTC.
    pub timestamp: UtcTimestamp,
    /// Native `techInfo` after credential redaction, at most 1024 characters.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "bounded_optional_text::<_, SECURITY_EVENT_TECH_INFO_MAX_CHARS_16>"
    )]
    #[schemars(length(max = 1024))]
    pub tech_info: Option<String>,
    /// Number of credential-like values replaced by the redaction marker in `tech_info`.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub tech_info_redactions: u16,
    /// Calls acknowledged but not stored by the flood gate since the previous durable record.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub shed_calls: u32,
}
