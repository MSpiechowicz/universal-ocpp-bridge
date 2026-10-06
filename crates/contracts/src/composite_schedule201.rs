//! Indicative OCPP 2.0.1 composite schedules, separate from observed charging effects.
use crate::{ChargingSchedulePeriod201, ChargingScheduleRateUnit201, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Validated immutable context of the one native K08 query.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeScheduleRequest201 {
    /// Zero addresses the grid connection; positive IDs address the exact EVSE.
    pub evse_id: i32,
    /// Requested horizon in seconds.
    pub duration: i32,
    /// Optional forced unit; omission never invents a unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_rate_unit: Option<ChargingScheduleRateUnit201>,
}

/// Native `GenericStatusEnumType`, not evidence of physical enforcement.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum CompositeScheduleStatus201 {
    /// Charger returned its calculated schedule.
    Accepted,
    /// Charger could not report the requested schedule (K08.FR.05/07).
    Rejected,
}

/// Narrow standardized, case-insensitively matched reason codes. Opaque `additionalInfo`,
/// `customData` and unlisted codes are excluded.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SmartChargingReason201 {
    UnknownEvse,
    UnsupportedRateUnit,
    NotFound,
    DuplicateRequestId,
    InvalidValue,
    UnsupportedParam,
    UnsupportedRequest,
    InternalError,
}

impl SmartChargingReason201 {
    const ALL: [Self; 8] = [
        Self::UnknownEvse,
        Self::UnsupportedRateUnit,
        Self::NotFound,
        Self::DuplicateRequestId,
        Self::InvalidValue,
        Self::UnsupportedParam,
        Self::UnsupportedRequest,
        Self::InternalError,
    ];

    /// Native code spelling from the pinned appendix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownEvse => "UnknownEvse",
            Self::UnsupportedRateUnit => "UnsupportedRateUnit",
            Self::NotFound => "NotFound",
            Self::DuplicateRequestId => "DuplicateRequestId",
            Self::InvalidValue => "InvalidValue",
            Self::UnsupportedParam => "UnsupportedParam",
            Self::UnsupportedRequest => "UnsupportedRequest",
            Self::InternalError => "InternalError",
        }
    }

    /// Native `reasonCode` is case-insensitive; unlisted codes are not retained.
    #[must_use]
    pub fn from_native(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str().eq_ignore_ascii_case(code))
    }
}

/// Validated native response and original query context.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeScheduleResult201 {
    /// Context captured before transmission.
    pub request: CompositeScheduleRequest201,
    /// Exact native status.
    pub status: CompositeScheduleStatus201,
    /// Standardized native reason, only when supplied and listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<SmartChargingReason201>,
    /// Charger-calculated schedule, required for Accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<CompositeSchedule201>,
}

/// Native `CompositeScheduleType`; every field is required on the wire.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeSchedule201 {
    /// Exact requested EVSE or zero for the grid connection.
    pub evse_id: i32,
    /// Native horizon in seconds.
    pub duration: i32,
    /// Anchor of every relative period start.
    pub schedule_start: UtcTimestamp,
    /// Unit of all limits in this schedule.
    pub charging_rate_unit: ChargingScheduleRateUnit201,
    /// Ordered relative period boundaries, starting at zero.
    pub charging_schedule_period: Vec<ChargingSchedulePeriod201>,
}
