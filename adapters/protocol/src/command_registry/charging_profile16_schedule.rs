use std::borrow::Cow;

use rust_ocpp::v1_6::{
    messages::set_charging_profile::SetChargingProfileRequest, types::ChargingSchedule,
};
use serde::Deserialize;
use serde_json::Value;
use uob_contracts::{
    CompositeSchedule16, CompositeSchedulePeriod16, CompositeScheduleRateUnit16, ExactDecimal,
    UtcTimestamp,
};

use crate::v16::remote_control::exact_rate::{decoder_rate, exact_rate};

pub(super) const MAX_PERIODS: usize = 1024;

pub(super) fn decode(payload: &Value) -> Option<SetChargingProfileRequest> {
    let schedule = &payload["csChargingProfiles"]["chargingSchedule"];
    let periods = schedule["chargingSchedulePeriod"].as_array()?;
    if periods.is_empty() || periods.len() > MAX_PERIODS {
        return None;
    }

    // Normalize only decoder-incompatible lexemes. Preserve the original wire payload and
    // compare every decoded rate to its exact original value before retaining evidence.
    let mut normalized = Cow::Borrowed(payload);
    for (index, period) in periods.iter().enumerate() {
        if let Cow::Owned(rate) = decoder_rate(&period["limit"])? {
            normalized.to_mut()["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"]
                [index]["limit"] = rate;
        }
    }
    if let Some(rate) = schedule.get("minChargingRate")
        && let Cow::Owned(rate) = decoder_rate(rate)?
    {
        normalized.to_mut()["csChargingProfiles"]["chargingSchedule"]["minChargingRate"] = rate;
    }
    SetChargingProfileRequest::deserialize(normalized.as_ref()).ok()
}

pub(super) fn parse(raw: &Value, native: ChargingSchedule) -> Option<CompositeSchedule16> {
    let periods = raw["chargingSchedulePeriod"].as_array()?;
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
        CompositeScheduleRateUnit16::deserialize(&raw["chargingRateUnit"]).ok()?;

    let mut previous = None;
    let mut result = Vec::with_capacity(periods.len());
    for (raw, native) in periods.iter().zip(native.charging_schedule_period) {
        if native.start_period < 0
            || previous.map_or(native.start_period != 0, |start| {
                native.start_period <= start
            })
            || native.number_phases.is_some_and(|phases| phases <= 0)
        {
            return None;
        }
        let limit = exact_rate(&raw["limit"])?;
        if limit != ExactDecimal::new(native.limit.mantissa(), native.limit.scale()) {
            return None;
        }
        previous = Some(native.start_period);
        result.push(CompositeSchedulePeriod16 {
            start_period: native.start_period,
            limit,
            number_phases: native.number_phases,
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
    // Unlike composite replies, profile periods beyond duration/recurrence are legal,
    // including the native zero-duration boundary. The charger truncates their execution.
    Some(CompositeSchedule16 {
        duration,
        start_schedule,
        charging_rate_unit,
        charging_schedule_period: result,
        min_charging_rate,
    })
}
