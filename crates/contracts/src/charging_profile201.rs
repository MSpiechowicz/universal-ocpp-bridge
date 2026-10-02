//! Immutable OCPP 2.0.1 profile requests and acknowledgements, not enforcement evidence.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
mod schedule;
pub use schedule::*;

/// Native CSMS-owned profile purpose. External constraints cannot be set or cleared here.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfilePurpose201 {
    ChargingStationMaxProfile,
    TxDefaultProfile,
    TxProfile,
}

/// Native schedule anchor semantics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfileKind201 {
    Absolute,
    Recurring,
    Relative,
}

/// Native recurrence interval.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfileRecurrency201 {
    Daily,
    Weekly,
}

/// Complete validated profile. Omitted validity remains indefinite.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfile201 {
    pub id: i32,
    pub stack_level: i32,
    pub charging_profile_purpose: ChargingProfilePurpose201,
    pub charging_profile_kind: ChargingProfileKind201,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrency_kind: Option<ChargingProfileRecurrency201>,
    /// Inclusive lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<UtcTimestamp>,
    /// Exclusive upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<UtcTimestamp>,
    /// Exactly one schedule in the supported non-ISO context.
    pub charging_schedule: Vec<ChargingSchedule201>,
}

/// Immutable native Set request; zero addresses the station, not all EVSEs.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SetChargingProfileRequest201 {
    pub evse_id: i32,
    pub charging_profile: ChargingProfile201,
}

/// Native AND filters; absent EVSE addresses all EVSEs, zero only station profiles.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfileCriteria201 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_profile_purpose: Option<ChargingProfilePurpose201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_level: Option<i32>,
}

/// ID alone or at least one nested criterion, never both.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ClearChargingProfileRequest201 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_profile_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_profile_criteria: Option<ChargingProfileCriteria201>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SetChargingProfileStatus201 {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ClearChargingProfileStatus201 {
    Accepted,
    Unknown,
}

/// Narrow safe reason codes. Opaque additionalInfo, customData and unknown reasons are excluded.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfileReason201 {
    NotSupported,
    InvalidValue,
    NotFound,
}

/// Full native request plus a valid correlated CALLRESULT, never a CALLERROR or uncertainty.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action")]
pub enum ChargingProfileResult201 {
    SetChargingProfile {
        request: SetChargingProfileRequest201,
        status: SetChargingProfileStatus201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<ChargingProfileReason201>,
    },
    ClearChargingProfile {
        request: ClearChargingProfileRequest201,
        status: ClearChargingProfileStatus201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<ChargingProfileReason201>,
    },
}

impl ChargingProfileResult201 {
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(
            self,
            Self::SetChargingProfile {
                status: SetChargingProfileStatus201::Accepted,
                ..
            } | Self::ClearChargingProfile {
                status: ClearChargingProfileStatus201::Accepted,
                ..
            }
        )
    }
}
