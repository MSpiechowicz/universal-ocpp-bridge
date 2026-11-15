//! Pure rules of the security model: the pinned `SecurityEventNotification.req` bounds and the
//! `ExtendedTriggerMessage.req` request shape (Security Whitepaper Edition 4 §5.5, §5.15).
use super::config::ExtendedTriggerConfig;
use crate::TriggerReply;
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// `type` is `maxLength` 50 in the pinned schema.
pub const TYPE_LIMIT: usize = 50;
/// `techInfo` is `maxLength` 255 in the pinned schema.
pub const TECH_INFO_LIMIT: usize = 255;

/// Enforces the pinned `SecurityEventNotification.req` bounds before an event is queued, so an
/// over-long field fails locally with its own error instead of a generic one.
///
/// # Errors
/// A stable code naming the first violated bound; lengths count characters like the schema.
pub fn validate_event(
    kind: &str,
    timestamp: &str,
    tech_info: Option<&str>,
) -> Result<(), &'static str> {
    if kind.chars().count() > TYPE_LIMIT {
        return Err("security_event_type_too_long");
    }
    if OffsetDateTime::parse(timestamp, &Rfc3339).is_err() {
        return Err("security_event_timestamp_invalid");
    }
    if tech_info.is_some_and(|text| text.chars().count() > TECH_INFO_LIMIT) {
        return Err("security_event_tech_info_too_long");
    }
    Ok(())
}

/// `MessageTriggerEnumType` of the pinned `ExtendedTriggerMessage.req`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum ExtendedMessage {
    BootNotification,
    LogStatusNotification,
    FirmwareStatusNotification,
    Heartbeat,
    MeterValues,
    SignChargePointCertificate,
    StatusNotification,
}

impl ExtendedMessage {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::BootNotification => "BootNotification",
            Self::LogStatusNotification => "LogStatusNotification",
            Self::FirmwareStatusNotification => "FirmwareStatusNotification",
            Self::Heartbeat => "Heartbeat",
            Self::MeterValues => "MeterValues",
            Self::SignChargePointCertificate => "SignChargePointCertificate",
            Self::StatusNotification => "StatusNotification",
        }
    }

    pub(crate) const fn reply(self, config: &ExtendedTriggerConfig) -> TriggerReply {
        match self {
            Self::BootNotification => config.boot_notification,
            Self::LogStatusNotification => config.log_status_notification,
            Self::FirmwareStatusNotification => config.firmware_status_notification,
            Self::Heartbeat => config.heartbeat,
            Self::MeterValues => config.meter_values,
            Self::SignChargePointCertificate => config.sign_charge_point_certificate,
            Self::StatusNotification => config.status_notification,
        }
    }
}

pub(crate) const fn status_name(reply: TriggerReply) -> &'static str {
    match reply {
        TriggerReply::Accepted => "Accepted",
        TriggerReply::Rejected => "Rejected",
        TriggerReply::NotImplemented => "NotImplemented",
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    #[serde(rename = "requestedMessage")]
    requested_message: ExtendedMessage,
    #[serde(rename = "connectorId")]
    connector_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExtendedRequest {
    pub(crate) message: ExtendedMessage,
    pub(crate) connector_id: Option<i64>,
}

/// An unknown field or value is a `FormationViolation`; a `connectorId` that is not above zero
/// breaks the whitepaper's own constraint and is a `PropertyConstraintViolation`.
pub(crate) fn parse_request(payload: &serde_json::Value) -> Result<ExtendedRequest, &'static str> {
    let raw: RawRequest =
        serde_json::from_value(payload.clone()).map_err(|_| "FormationViolation")?;
    if raw.connector_id.is_some_and(|id| id <= 0) {
        return Err("PropertyConstraintViolation");
    }
    Ok(ExtendedRequest {
        message: raw.requested_message,
        connector_id: raw.connector_id,
    })
}
