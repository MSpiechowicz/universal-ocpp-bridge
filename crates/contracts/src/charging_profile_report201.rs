//! OCPP 2.0.1 installed-profile queries (K09) and their bounded native report evidence.
//! Reported profiles describe charger state at report time; they are never local policy.
use crate::{
    ChargingProfileKind201, ChargingProfileRecurrency201, ChargingSchedule201,
    SmartChargingReason201, UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Bound for one durable report, matching the shared multipart collection limit.
pub const CHARGING_PROFILE_REPORT_OUTPUT_LIMIT_201: usize = 1024 * 1024;

/// Native `ChargingLimitSourceEnumType`: who installed the profile.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum ChargingLimitSource201 {
    #[serde(rename = "EMS")]
    Ems,
    Other,
    #[serde(rename = "SO")]
    So,
    #[serde(rename = "CSO")]
    Cso,
}

/// Every native purpose, including externally installed constraints.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ReportedChargingProfilePurpose201 {
    ChargingStationExternalConstraints,
    ChargingStationMaxProfile,
    TxDefaultProfile,
    TxProfile,
}

/// Validated native `GetChargingProfilesRequest`. Either profile IDs or at least one other
/// criterion is present (K09.FR.03); every supplied field must match a reported profile.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfilesQuery201 {
    /// Signed native report correlation identity, separate from the CALL ID.
    pub request_id: i32,
    /// Absent reports every EVSE; zero only the grid connection (K09.FR.04-06).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_profile_purpose: Option<ReportedChargingProfilePurpose201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_level: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charging_profile_id: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charging_limit_source: Vec<ChargingLimitSource201>,
}

/// Native `GetChargingProfileStatusEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingProfilesStatus201 {
    /// Reports follow as `ReportChargingProfilesRequest`.
    Accepted,
    /// No installed profile matched; no report is expected.
    NoProfiles,
}

/// One installed profile with its reporting EVSE and source. `customData` is never retained;
/// a supplied ISO 15118 `salesTariff` is omitted and flagged rather than interpreted.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ReportedChargingProfile201 {
    /// Native EVSE of the report fragment; zero is the grid connection.
    pub evse_id: i32,
    pub charging_limit_source: ChargingLimitSource201,
    pub id: i32,
    pub stack_level: i32,
    pub charging_profile_purpose: ReportedChargingProfilePurpose201,
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
    /// One to three native schedules with exact limits.
    pub charging_schedule: Vec<ChargingSchedule201>,
    /// True when at least one schedule carried an omitted native `salesTariff`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sales_tariff_omitted: bool,
}

/// Metadata for every accepted native fragment, in arrival order.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfileReportFragment201 {
    /// Arrival position; the native message has no sequence number.
    pub sequence: u32,
    pub evse_id: i32,
    pub charging_limit_source: ChargingLimitSource201,
    pub more: bool,
    pub profiles: usize,
}

/// Counts include only accepted fragments; recovered unavailable counts remain absent.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfileReportProgress201 {
    pub fragments: u32,
    pub profiles: usize,
    pub bytes: usize,
}

/// Payload-free terminal reason.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargingProfileReportFailure201 {
    Timeout,
    ByteLimit,
    ItemLimit,
    FragmentLimit,
    Capacity,
    Correlation,
    InvalidFragment,
    Disconnected,
    Interrupted,
    OutputLimit,
    StorageUnavailable,
}

/// Collection result, recorded only after the native acknowledgement.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChargingProfileReportState201 {
    Pending,
    Complete {
        progress: ChargingProfileReportProgress201,
        fragments: Vec<ChargingProfileReportFragment201>,
        profiles: Vec<ReportedChargingProfile201>,
    },
    Incomplete {
        reason: ChargingProfileReportFailure201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<ChargingProfileReportProgress201>,
    },
    NotExpected,
}

impl ChargingProfileReportState201 {
    #[must_use]
    pub const fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Native acknowledgement plus separately collected report evidence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingProfilesResult201 {
    /// Context captured before transmission.
    pub query: ChargingProfilesQuery201,
    /// Exact native status.
    pub status: ChargingProfilesStatus201,
    /// Standardized native reason, only when supplied and listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<SmartChargingReason201>,
    /// Pending after Accepted until the final fragment, a limit or the deadline.
    pub report: ChargingProfileReportState201,
}
