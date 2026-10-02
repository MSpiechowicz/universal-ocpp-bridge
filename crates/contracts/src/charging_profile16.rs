//! Immutable native profile requests and acknowledgements, not enforcement evidence.
use crate::{CompositeSchedule16, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Native profile purpose.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfilePurpose16 {
    /// Station aggregate ceiling.
    ChargePointMaxProfile,
    /// Default for future transactions.
    TxDefaultProfile,
    /// A specific ongoing transaction.
    TxProfile,
}

/// Native schedule anchor semantics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfileKind16 {
    /// Absolute time anchor.
    Absolute,
    /// Repeating schedule.
    Recurring,
    /// Relative to transaction start.
    Relative,
}

/// Native repetition interval.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfileRecurrency16 {
    /// Daily repetition.
    Daily,
    /// Weekly repetition.
    Weekly,
}

/// Complete supplied profile identity and schedule.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfile16 {
    /// Signed native profile identity.
    pub charging_profile_id: i32,
    /// Exact ongoing native transaction, only for `TxProfile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<i32>,
    /// Nonnegative priority stack.
    pub stack_level: i32,
    /// Native purpose.
    pub charging_profile_purpose: ChargingProfilePurpose16,
    /// Native anchor semantics.
    pub charging_profile_kind: ChargingProfileKind16,
    /// Optional recurrence; allowed only for recurring profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrency_kind: Option<ChargingProfileRecurrency16>,
    /// Supplied validity lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<UtcTimestamp>,
    /// Supplied validity upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<UtcTimestamp>,
    /// Exact native A/W schedule; legal truncated periods remain present.
    pub charging_schedule: CompositeSchedule16,
}

/// Immutable Set request captured before enqueue.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SetChargingProfileRequest16 {
    /// Zero denotes station, positive denotes exact connector.
    pub connector_id: i32,
    /// Complete supplied native profile.
    pub cs_charging_profiles: ChargingProfile16,
}

/// Supplied Clear selectors. ID overrides all other supplied filters on the charger.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ClearChargingProfileRequest16 {
    /// Optional signed native ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    /// Optional native connector filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<i32>,
    /// Optional purpose filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_profile_purpose: Option<ChargingProfilePurpose16>,
    /// Optional nonnegative stack filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_level: Option<i32>,
}

/// Set native acknowledgement.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SetChargingProfileStatus16 {
    /// Profile accepted; no enforcement claim.
    Accepted,
    /// Profile declined.
    Rejected,
    /// Operation/profile unsupported by charger.
    NotSupported,
}

/// Clear native acknowledgement; removed identities are not returned.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ClearChargingProfileStatus16 {
    /// Charger acknowledged clear.
    Accepted,
    /// No matching profile known.
    Unknown,
}

/// Action-specific immutable request and valid native CALLRESULT.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action")]
pub enum ChargingProfileResult16 {
    /// Full privileged native Set, never canonical `SetChargingLimit`.
    SetChargingProfile {
        /// Validated original request.
        request: SetChargingProfileRequest16,
        /// Exact native status.
        status: SetChargingProfileStatus16,
    },
    /// Native Clear; status does not reveal removed profile identities.
    ClearChargingProfile {
        /// Validated original selectors.
        request: ClearChargingProfileRequest16,
        /// Exact native status.
        status: ClearChargingProfileStatus16,
    },
}

impl ChargingProfileResult16 {
    /// Whether the native reply accepted this operation.
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(
            self,
            Self::SetChargingProfile {
                status: SetChargingProfileStatus16::Accepted,
                ..
            } | Self::ClearChargingProfile {
                status: ClearChargingProfileStatus16::Accepted,
                ..
            }
        )
    }
}
