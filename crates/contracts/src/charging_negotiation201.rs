//! Charger-initiated OCPP 2.0.1 charging needs, EV schedules and external charging limits
//! (K11-K17) with the bridge's own native answer. These are observed facts and evidence only:
//! they never become commands, local policy or proof of physical charging behavior.
use crate::{ChargingLimitSource201, ChargingSchedule201, TransactionId, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Bound for one journaled negotiation record. Schedules that would exceed it are omitted and
/// counted, so the record always fits the integration event stream's payload limit.
pub const CHARGING_NEGOTIATION_EVIDENCE_LIMIT_201: usize = 32 * 1024;

/// Native `EnergyTransferModeEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum EnergyTransferMode201 {
    #[serde(rename = "DC")]
    Dc,
    #[serde(rename = "AC_single_phase")]
    AcSinglePhase,
    #[serde(rename = "AC_two_phase")]
    AcTwoPhase,
    #[serde(rename = "AC_three_phase")]
    AcThreePhase,
}

impl EnergyTransferMode201 {
    /// Native enumeration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dc => "DC",
            Self::AcSinglePhase => "AC_single_phase",
            Self::AcTwoPhase => "AC_two_phase",
            Self::AcThreePhase => "AC_three_phase",
        }
    }
}

/// Native `ACChargingParametersType`: Wh, amperes per phase and volts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AcChargingParameters201 {
    pub energy_amount: u32,
    pub ev_min_current: u32,
    pub ev_max_current: u32,
    pub ev_max_voltage: u32,
}

/// Native `DCChargingParametersType`: amperes, volts, Wh, W and percentages.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DcChargingParameters201 {
    pub ev_max_current: u32,
    pub ev_max_voltage: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_amount: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ev_max_power: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_of_charge: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ev_energy_capacity: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_soc: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bulk_soc: Option<u8>,
}

/// Exactly one parameter set, matching the requested transfer mode (K15.FR.06, K17.FR.06).
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvChargingParameters201 {
    Ac(AcChargingParameters201),
    Dc(DcChargingParameters201),
}

/// Validated native `NotifyEVChargingNeedsRequest`; `customData` is not retained.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct EvChargingNeeds201 {
    /// Positive native EVSE identity.
    pub evse_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_schedule_tuples: Option<u32>,
    pub requested_energy_transfer: EnergyTransferMode201,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<UtcTimestamp>,
    pub parameters: EvChargingParameters201,
}

/// Native `NotifyEVChargingNeedsStatusEnumType` answered by the bridge.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum EvChargingNeedsStatus201 {
    /// The bridge never claims a schedule is available now; retained for completeness.
    Accepted,
    /// No schedule will follow from this bridge (K15.FR.04, K17.FR.04).
    Rejected,
    /// The operator's EMS provides a `TxProfile` later (K15.FR.05, K17.FR.05).
    Processing,
}

impl EvChargingNeedsStatus201 {
    /// Native enumeration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
            Self::Processing => "Processing",
        }
    }
}

/// Native `GenericStatusEnumType` answered for an EV charging schedule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum EvChargingScheduleStatus201 {
    Accepted,
    Rejected,
}

impl EvChargingScheduleStatus201 {
    /// Native enumeration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
        }
    }
}

/// Standardized appendix reason codes the bridge itself sends in `statusInfo`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum NegotiationReason201 {
    UnknownEvse,
    TxNotFound,
    NotEnabled,
    ValueTooHigh,
    Unspecified,
}

impl NegotiationReason201 {
    /// Native code spelling from the pinned appendix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownEvse => "UnknownEvse",
            Self::TxNotFound => "TxNotFound",
            Self::NotEnabled => "NotEnabled",
            Self::ValueTooHigh => "ValueTooHigh",
            Self::Unspecified => "Unspecified",
        }
    }
}

/// Validated native `NotifyEVChargingScheduleRequest`. Periods are relative to `time_base`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct EvChargingSchedule201 {
    /// Positive native EVSE identity.
    pub evse_id: u32,
    pub time_base: UtcTimestamp,
    /// Absent only when the schedule exceeds the evidence bound; see `period_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_schedule: Option<ChargingSchedule201>,
    /// Native period count, retained even when the schedule itself is omitted.
    pub period_count: u32,
    /// An ISO 15118 `salesTariff` was supplied and not retained.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sales_tariff_omitted: bool,
}

/// Basis of the bridge's own check (K15.FR.11/12, K16.FR.06/07, K17.FR.11/12).
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvScheduleBasis201 {
    /// The bridge installed no `TxProfile` for this transaction.
    NoCsmsSchedule,
    /// Every EV period is within the bridge-installed `TxProfile` stack.
    WithinCsmsSchedule,
    /// At least one EV period exceeds the bridge-installed `TxProfile` stack.
    ExceedsCsmsSchedule,
    /// Installed limits are not exactly known: uncertain ownership, another unit,
    /// recurrence, or relative timing that the bridge cannot place.
    Unverifiable,
    /// The EVSE is not configured.
    UnknownEvse,
    /// The EVSE has no single current transaction.
    NoTransaction,
}

impl EvScheduleBasis201 {
    /// Serialized spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoCsmsSchedule => "no_csms_schedule",
            Self::WithinCsmsSchedule => "within_csms_schedule",
            Self::ExceedsCsmsSchedule => "exceeds_csms_schedule",
            Self::Unverifiable => "unverifiable",
            Self::UnknownEvse => "unknown_evse",
            Self::NoTransaction => "no_transaction",
        }
    }
}

/// Validated native `NotifyChargingLimitRequest`, never CSO-sourced (K11.FR.05, K12.FR.04).
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ExternalChargingLimit201 {
    /// Positive native EVSE; absent addresses the station's grid connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse_id: Option<u32>,
    pub charging_limit_source: ChargingLimitSource201,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_grid_critical: Option<bool>,
    /// Exact external schedules in native order; empty when none was sent or all were omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charging_schedule: Vec<ChargingSchedule201>,
    /// Count of schedules not retained because they exceed the evidence bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedules_omitted: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sales_tariff_omitted: bool,
}

/// Validated native `ClearedChargingLimitRequest`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ClearedChargingLimit201 {
    pub charging_limit_source: ChargingLimitSource201,
    /// Exactly as reported; absent or zero addresses the station's grid connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse_id: Option<u32>,
}

/// One committed charger notification and the bridge's native answer, journaled atomically
/// with the snapshot points it changed.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChargingNegotiation201 {
    EvChargingNeeds {
        needs: EvChargingNeeds201,
        status: EvChargingNeedsStatus201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<NegotiationReason201>,
        /// Canonical transaction on the EVSE when exactly one was current.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transaction_id: Option<TransactionId>,
    },
    EvChargingSchedule {
        schedule: EvChargingSchedule201,
        status: EvChargingScheduleStatus201,
        basis: EvScheduleBasis201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<NegotiationReason201>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transaction_id: Option<TransactionId>,
    },
    ChargingLimit {
        limit: ExternalChargingLimit201,
    },
    ChargingLimitCleared {
        cleared: ClearedChargingLimit201,
        /// A matching limit was active before this notification.
        released: bool,
    },
}

// Serde passes a reference to `skip_serializing_if` predicates.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}
