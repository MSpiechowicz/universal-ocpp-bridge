//! Charger-initiated OCPP 1.6 Security Whitepaper `SecurityEventNotification` (section 5.15):
//! strict native decoding into an observed fact. The CSMS answers every schema-valid
//! notification with the empty response, so beyond the pinned schema nothing is refused.
//!
//! The raw `techInfo` ends inside this module: the observation constructor redacts it, the
//! request struct has no `Debug`, and a decode failure carries no payload text.
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uob_application::{ChargerObservation, SecurityEventObservation16};
use uob_contracts::{ProtocolEdition, UtcTimestamp};

use crate::{DecodeError, DecodeErrorKind};

const PROTOCOL: ProtocolEdition = ProtocolEdition::Ocpp16j;

/// Exact `SecurityEventNotification.req`. Deliberately not `Debug`: `tech_info` is raw.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: UtcTimestamp,
    #[serde(default, rename = "techInfo", deserialize_with = "present_tech_info")]
    tech_info: Option<String>,
}

/// An omitted `techInfo` is absent, but an explicit `null` is not a string.
fn present_tech_info<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn invalid() -> DecodeError {
    DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
}

/// Decodes one `SecurityEventNotification` CALL payload into a redacted observation.
pub(super) fn observation(payload: Value) -> Result<ChargerObservation, DecodeError> {
    let request: Request = serde_json::from_value(payload).map_err(|_| invalid())?;
    let observation = SecurityEventObservation16::new(
        &request.event_type,
        request.timestamp,
        request.tech_info.as_deref(),
    )
    .map_err(|_| invalid())?;

    Ok(ChargerObservation::SecurityEvent16(observation))
}

/// The exact native response: the empty `SecurityEventNotificationResponse` object.
#[must_use]
pub fn response() -> Value {
    json!({})
}
