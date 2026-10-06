//! Strict raw native schedule parsing shared by composite schedules and installed-profile reports.
//! Callers validate the pinned OCA schema first; these checks add exactness and ordering.
use crate::v16::remote_control::exact_rate::exact_rate;
use serde::Deserialize;
use serde_json::Value;
use uob_contracts::{
    ChargingSchedule201, ChargingSchedulePeriod201, ChargingScheduleRateUnit201, UtcTimestamp,
};

pub(super) fn integer(value: &Value) -> Option<i32> {
    i32::try_from(value.as_i64()?).ok()
}

/// A supplied optional field that is not a valid value.
pub(super) struct Invalid;

fn optional_integer(value: Option<&Value>) -> Result<Option<i32>, Invalid> {
    value.map_or(Ok(None), |value| integer(value).map(Some).ok_or(Invalid))
}

pub(super) fn timestamp(value: Option<&Value>) -> Result<Option<UtcTimestamp>, Invalid> {
    value
        .map(UtcTimestamp::deserialize)
        .transpose()
        .map_err(|_| Invalid)
}

/// The schedule anchor plus its horizon must remain representable.
pub(super) fn horizon(start: UtcTimestamp, seconds: i32) -> Option<()> {
    let duration = std::time::Duration::from_secs(u64::try_from(seconds).ok()?)
        .try_into()
        .ok()?;
    start.into_inner().checked_add(duration).map(|_| ())
}

/// Periods start at zero, strictly increase, stay below `horizon` when given, and carry exact
/// nonnegative tenths. `numberPhases` is 1–3 and `phaseToUse` is allowed only for one phase.
pub(super) fn periods(raw: &Value, horizon: Option<i32>) -> Option<Vec<ChargingSchedulePeriod201>> {
    let raw = raw.as_array().filter(|periods| !periods.is_empty())?;
    let mut previous = None;
    let mut periods = Vec::with_capacity(raw.len());
    for period in raw {
        let start_period = integer(&period["startPeriod"])?;
        let number_phases = optional_integer(period.get("numberPhases")).ok()?;
        let phase_to_use = optional_integer(period.get("phaseToUse")).ok()?;
        if start_period < 0
            || previous.map_or(start_period != 0, |start| start_period <= start)
            || horizon.is_some_and(|seconds| start_period >= seconds)
            || number_phases.is_some_and(|phases| !(1..=3).contains(&phases))
            || phase_to_use
                .is_some_and(|phase| number_phases != Some(1) || !(1..=3).contains(&phase))
        {
            return None;
        }
        previous = Some(start_period);
        periods.push(ChargingSchedulePeriod201 {
            start_period,
            limit: exact_rate(&period["limit"])?,
            number_phases,
            phase_to_use,
        });
    }
    Some(periods)
}

/// One native `ChargingScheduleType`. Returns whether an ISO 15118 `salesTariff` was omitted.
pub(crate) fn schedule(raw: &Value) -> Option<(ChargingSchedule201, bool)> {
    let duration = optional_integer(raw.get("duration")).ok()?;
    if duration.is_some_and(|seconds| seconds < 0) {
        return None;
    }
    let start_schedule = timestamp(raw.get("startSchedule")).ok()?;
    if let (Some(start), Some(seconds)) = (start_schedule, duration) {
        horizon(start, seconds)?;
    }
    let min_charging_rate = raw.get("minChargingRate").map(exact_rate);
    if min_charging_rate.as_ref().is_some_and(Option::is_none) {
        return None;
    }
    Some((
        ChargingSchedule201 {
            id: integer(&raw["id"])?,
            duration,
            start_schedule,
            charging_rate_unit: ChargingScheduleRateUnit201::deserialize(&raw["chargingRateUnit"])
                .ok()?,
            // Native duration may legally truncate later periods; they are retained.
            charging_schedule_period: periods(&raw["chargingSchedulePeriod"], None)?,
            min_charging_rate: min_charging_rate.flatten(),
        },
        raw.get("salesTariff").is_some(),
    ))
}
