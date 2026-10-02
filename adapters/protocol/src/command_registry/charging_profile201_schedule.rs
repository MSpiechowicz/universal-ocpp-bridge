use super::charging_profile201::integer;
use crate::v16::remote_control::exact_rate::{decoder_rate, exact_rate};
use rust_ocpp::v2_0_1::{
    datatypes::charging_schedule_type::ChargingScheduleType,
    messages::set_charging_profile::SetChargingProfileRequest,
};
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use uob_contracts::{
    ChargingSchedule201, ChargingSchedulePeriod201, ChargingScheduleRateUnit201, ExactDecimal,
    UtcTimestamp,
};

pub(super) fn decode(payload: &Value) -> Option<SetChargingProfileRequest> {
    let schedules = payload["chargingProfile"]["chargingSchedule"].as_array()?;
    if schedules.len() != 1 {
        return None;
    }
    let schedule = &schedules[0];
    let periods = schedule["chargingSchedulePeriod"].as_array()?;
    if periods.is_empty() || periods.len() > 1024 {
        return None;
    }
    let mut normalized = Cow::Borrowed(payload);
    for (index, period) in periods.iter().enumerate() {
        if let Cow::Owned(rate) = decoder_rate(&period["limit"])? {
            normalized.to_mut()["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"]
                [index]["limit"] = rate;
        }
    }
    if let Some(rate) = schedule.get("minChargingRate")
        && let Cow::Owned(rate) = decoder_rate(rate)?
    {
        normalized.to_mut()["chargingProfile"]["chargingSchedule"][0]["minChargingRate"] = rate;
    }
    SetChargingProfileRequest::deserialize(normalized.as_ref()).ok()
}

pub(super) fn parse(raw: &Value, native: ChargingScheduleType) -> Option<ChargingSchedule201> {
    let id = integer(&raw["id"])?;
    let duration = native.duration;
    if duration.is_some_and(|seconds| seconds < 0) {
        return None;
    }
    let start_schedule = raw
        .get("startSchedule")
        .map(UtcTimestamp::deserialize)
        .transpose()
        .ok()?;
    let charging_rate_unit =
        ChargingScheduleRateUnit201::deserialize(&raw["chargingRateUnit"]).ok()?;
    let raw_periods = raw["chargingSchedulePeriod"].as_array()?;
    if raw_periods.len() != native.charging_schedule_period.len() {
        return None;
    }
    let mut previous = None;
    let mut periods = Vec::with_capacity(raw_periods.len());
    for (raw, native) in raw_periods.iter().zip(native.charging_schedule_period) {
        if native.start_period < 0
            || previous.map_or(native.start_period != 0, |start| {
                native.start_period <= start
            })
            || native
                .number_phases
                .is_some_and(|phases| !(1..=3).contains(&phases))
            || native
                .phase_to_use
                .is_some_and(|phase| native.number_phases != Some(1) || !(1..=3).contains(&phase))
        {
            return None;
        }
        let limit = exact_rate(&raw["limit"])?;
        if limit != ExactDecimal::new(native.limit.mantissa(), native.limit.scale()) {
            return None;
        }
        previous = Some(native.start_period);
        periods.push(ChargingSchedulePeriod201 {
            start_period: native.start_period,
            limit,
            number_phases: native.number_phases,
            phase_to_use: native.phase_to_use,
        });
    }
    let min_charging_rate = match (raw.get("minChargingRate"), native.min_charging_rate) {
        (None, None) => None,
        (Some(raw), Some(native)) => {
            let rate = exact_rate(raw)?;
            if rate != ExactDecimal::new(native.mantissa(), native.scale()) {
                return None;
            }
            Some(rate)
        }
        _ => return None,
    };
    Some(ChargingSchedule201 {
        id,
        duration,
        start_schedule,
        charging_rate_unit,
        charging_schedule_period: periods,
        min_charging_rate,
    })
}
