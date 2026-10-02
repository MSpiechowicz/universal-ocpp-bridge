use crate::{ExactDecimal, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Native rate unit, never converted using guessed electrical characteristics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ChargingScheduleRateUnit201 {
    A,
    W,
}

/// Complete supplied native schedule, including legal duration-truncated periods.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingSchedule201 {
    pub id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_schedule: Option<UtcTimestamp>,
    pub charging_rate_unit: ChargingScheduleRateUnit201,
    pub charging_schedule_period: Vec<ChargingSchedulePeriod201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_charging_rate: Option<ExactDecimal>,
}

/// One exact nonnegative native tenths rate; omitted phases remain absent.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ChargingSchedulePeriod201 {
    pub start_period: i32,
    pub limit: ExactDecimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_phases: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_to_use: Option<i32>,
}
