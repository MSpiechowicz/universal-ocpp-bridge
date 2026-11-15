//! Application-owned OCPP 1.6 Security Whitepaper `SecurityEventNotification` evidence. Events
//! are charger facts, journaled as observed and never turned into configuration, policy or
//! commands.
//!
//! The free-form `techInfo` is redacted when the observation is built, so no raw charger text
//! enters the application. Storage retention cannot shed journal events, so flooding is stopped
//! before the write by [`SecurityEventGate16`], and a redelivered event is recognised through
//! [`SecurityEventStore16`] before it spends any budget.
mod gate;
mod persistence;
mod redaction;
pub use gate::{
    SecurityEventDecision16, SecurityEventGate16, SecurityEventLimits16, SecurityEventReceipt16,
    record_security_event_shed_16,
};
pub use persistence::record_security_event_16;
pub use redaction::{REDACTION_MARKER, RedactedTechInfo16, redact_tech_info_16};

use crate::StorageFuture;
use std::{error::Error, fmt};
use uob_contracts::{ResourceRef, SECURITY_EVENT_TYPE_MAX_CHARS_16, SecurityEvent16, UtcTimestamp};

/// Native `SecurityEventNotification.req` `techInfo` limit.
pub const SECURITY_EVENT_NATIVE_TECH_INFO_MAX_CHARS_16: usize = 255;

/// The event types of Security Whitepaper section 8. Any other type is a vendor event.
pub const STANDARD_SECURITY_EVENTS_16: [&str; 21] = [
    "FirmwareUpdated",
    "FailedToAuthenticateAtCentralSystem",
    "CentralSystemFailedToAuthenticate",
    "SettingSystemTime",
    "StartupOfTheDevice",
    "ResetOrReboot",
    "SecurityLogWasCleared",
    "ReconfigurationOfSecurityParameters",
    "MemoryExhaustion",
    "InvalidMessages",
    "AttemptedReplayAttacks",
    "TamperDetectionActivated",
    "InvalidFirmwareSignature",
    "InvalidFirmwareSigningCertificate",
    "InvalidCentralSystemCertificate",
    "InvalidChargePointCertificate",
    "InvalidTLSVersion",
    "InvalidTLSCipherSuite",
    "MaintenanceLoginAccepted",
    "MaintenanceLoginFailed",
    "DiscardedRenewedClientCertificate",
];

/// Whether `event_type` is exactly one of the standard section 8 names (case-sensitive).
#[must_use]
pub fn is_standard_security_event_16(event_type: &str) -> bool {
    STANDARD_SECURITY_EVENTS_16.contains(&event_type)
}

#[cfg(test)]
mod tests;

/// A native field exceeded its schema bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityEventError16 {
    /// `type` is longer than 50 characters.
    TypeTooLong,
    /// `techInfo` is longer than 255 characters.
    TechInfoTooLong,
}

impl fmt::Display for SecurityEventError16 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TypeTooLong => "security event type exceeds its native bound",
            Self::TechInfoTooLong => "security event techInfo exceeds its native bound",
        })
    }
}

impl Error for SecurityEventError16 {}

/// The content of one schema-valid `SecurityEventNotification` CALL. `techInfo` is redacted by
/// the constructor and the raw text is not kept, so the observation is safe to log, trace and
/// store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityEventObservation16 {
    event_type: String,
    timestamp: UtcTimestamp,
    tech_info: Option<String>,
    tech_info_redactions: u16,
}

impl SecurityEventObservation16 {
    /// Validates the native bounds and redacts `raw_tech_info`. A vendor-specific `event_type`
    /// is accepted verbatim; an empty one is schema-valid. The `timestamp` is already UTC.
    ///
    /// # Errors
    ///
    /// Rejects a `type` over 50 characters or a `techInfo` over 255 characters.
    pub fn new(
        event_type: &str,
        timestamp: UtcTimestamp,
        raw_tech_info: Option<&str>,
    ) -> Result<Self, SecurityEventError16> {
        if event_type.chars().count() > SECURITY_EVENT_TYPE_MAX_CHARS_16 {
            return Err(SecurityEventError16::TypeTooLong);
        }
        if raw_tech_info
            .is_some_and(|text| text.chars().count() > SECURITY_EVENT_NATIVE_TECH_INFO_MAX_CHARS_16)
        {
            return Err(SecurityEventError16::TechInfoTooLong);
        }

        let redacted = raw_tech_info.map(redact_tech_info_16);
        let tech_info_redactions = redacted.as_ref().map_or(0, |tech| tech.redactions);

        Ok(Self {
            event_type: event_type.to_owned(),
            timestamp,
            tech_info: redacted.map(|tech| tech.text),
            tech_info_redactions,
        })
    }

    #[must_use]
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    #[must_use]
    pub const fn timestamp(&self) -> UtcTimestamp {
        self.timestamp
    }

    /// The redacted `techInfo`.
    #[must_use]
    pub fn tech_info(&self) -> Option<&str> {
        self.tech_info.as_deref()
    }

    #[must_use]
    pub const fn tech_info_redactions(&self) -> u16 {
        self.tech_info_redactions
    }

    /// Whether the type is one of the standard section 8 names.
    #[must_use]
    pub fn standard(&self) -> bool {
        is_standard_security_event_16(&self.event_type)
    }

    /// Identity used to recognise a redelivery of this event from `station`.
    #[must_use]
    pub fn key(&self, station: ResourceRef) -> SecurityEventKey16 {
        SecurityEventKey16 {
            station,
            event_type: self.event_type.clone(),
            timestamp: self.timestamp,
            tech_info: self.tech_info.clone(),
        }
    }

    /// The journal payload, reporting `shed_calls` acknowledged-but-unstored calls.
    #[must_use]
    pub fn into_record(self, shed_calls: u32) -> SecurityEvent16 {
        SecurityEvent16 {
            standard: is_standard_security_event_16(&self.event_type),
            event_type: self.event_type,
            timestamp: self.timestamp,
            tech_info: self.tech_info,
            tech_info_redactions: self.tech_info_redactions,
            shed_calls,
        }
    }
}

/// What makes a redelivered event "the same": station, native `type`, UTC timestamp and the
/// redacted `techInfo`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityEventKey16 {
    pub station: ResourceRef,
    pub event_type: String,
    pub timestamp: UtcTimestamp,
    pub tech_info: Option<String>,
}

/// Dedup read of the durable journal, on the same worker as the atomic write.
pub trait SecurityEventStore16: Send + Sync {
    /// Whether a security event with exactly this key is already journaled (within journal
    /// retention).
    fn security_event_recorded_16(&self, key: SecurityEventKey16) -> StorageFuture<'_, bool>;
}
