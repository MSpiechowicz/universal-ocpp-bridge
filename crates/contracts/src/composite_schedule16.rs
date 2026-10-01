//! Indicative OCPP 1.6 composite schedules, separate from observed charging effects.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ExactDecimal, UtcTimestamp};

/// Validated immutable context of the one native schedule query.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeScheduleRequest16 {
    /// Zero addresses grid consumption; positive IDs address the exact connector.
    pub connector_id: i32,
    /// Requested horizon in seconds.
    pub duration: i32,
    /// Optional forced unit; omission never invents a unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_rate_unit: Option<CompositeScheduleRateUnit16>,
}

/// Native schedule request outcome, not evidence of physical enforcement.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum CompositeScheduleStatus16 {
    /// Charger returned a meaningful schedule.
    Accepted,
    /// Charger explicitly declined the query.
    Rejected,
}

/// Native current or power unit, never implicitly converted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum CompositeScheduleRateUnit16 {
    /// Amperes.
    A,
    /// Watts.
    W,
}

/// Validated native response and original query context.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeScheduleResult16 {
    /// Context captured before transmission.
    pub request: CompositeScheduleRequest16,
    /// Exact native status.
    pub status: CompositeScheduleStatus16,
    /// Native identity, only when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<i32>,
    /// Composite-period anchor, only when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_start: Option<UtcTimestamp>,
    /// Charger-calculated schedule, required for Accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_schedule: Option<CompositeSchedule16>,
}

/// Charger-calculated schedule at query time.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeSchedule16 {
    /// Native horizon, absent when not supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    /// Independent native schedule timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_schedule: Option<UtcTimestamp>,
    /// Unit of all rates in this schedule.
    pub charging_rate_unit: CompositeScheduleRateUnit16,
    /// Ordered relative period boundaries, starting at zero.
    pub charging_schedule_period: Vec<CompositeSchedulePeriod16>,
    /// Native minimum rate; it need not be below an off period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_charging_rate: Option<ExactDecimal>,
}

/// One exact native rate and optional phase count.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompositeSchedulePeriod16 {
    /// Relative start in seconds.
    pub start_period: i32,
    /// Exact nonnegative rate, encoded as a canonical decimal string.
    pub limit: ExactDecimal,
    /// Positive native phase count; omission remains absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_phases: Option<i32>,
}
