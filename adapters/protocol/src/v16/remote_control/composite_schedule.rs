use std::borrow::Cow;

use rust_ocpp::v1_6::messages::get_composite_schedule::GetCompositeScheduleResponse;
use serde::Deserialize;
use serde_json::{Map, Value};
use uob_application::CommandDispatchOutcome;
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, CompositeSchedule16, CompositeSchedulePeriod16,
    CompositeScheduleRateUnit16, CompositeScheduleRequest16, CompositeScheduleResult16,
    CompositeScheduleStatus16, ExactDecimal, UtcTimestamp,
};

pub(super) fn request_context(
    command: &Command<Value>,
    action: &str,
) -> Result<Option<CompositeScheduleRequest16>, CommandErrorCode> {
    if action != "GetCompositeSchedule" {
        return Ok(None);
    }
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Err(CommandErrorCode::InvalidParameters);
    };
    crate::command_registry::composite_schedule16::validate(&command.resource, operation).map(Some)
}

pub(super) fn response(
    request: CompositeScheduleRequest16,
    payload: &Value,
) -> CommandDispatchOutcome {
    parse(request, payload).map_or_else(
        super::mapping::uncertain,
        CommandDispatchOutcome::CompositeScheduleResponse16,
    )
}

fn object<'a>(
    value: &'a Value,
    allowed: &[&str],
    required: &[&str],
) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    if object
        .iter()
        .any(|(key, value)| !allowed.contains(&key.as_str()) || value.is_null())
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return None;
    }
    Some(object)
}

fn timestamp(value: Option<&Value>) -> Result<Option<UtcTimestamp>, serde_json::Error> {
    value.map(UtcTimestamp::deserialize).transpose()
}

fn horizon(timestamp: Option<UtcTimestamp>, seconds: i32) -> Option<()> {
    if let Some(timestamp) = timestamp {
        let duration = std::time::Duration::from_secs(u64::try_from(seconds).ok()?)
            .try_into()
            .ok()?;
        timestamp.into_inner().checked_add(duration)?;
    }
    Some(())
}

#[allow(clippy::too_many_lines)] // Validate the raw/native schedule as one request-aware boundary.
fn parse(
    request: CompositeScheduleRequest16,
    payload: &Value,
) -> Option<CompositeScheduleResult16> {
    let root = object(
        payload,
        &["status", "connectorId", "scheduleStart", "chargingSchedule"],
        &["status"],
    )?;
    let status = match root["status"].as_str()? {
        "Accepted" => CompositeScheduleStatus16::Accepted,
        "Rejected" => CompositeScheduleStatus16::Rejected,
        _ => return None,
    };
    let connector_id = match root.get("connectorId") {
        Some(value) => Some(i32::try_from(value.as_i64()?).ok()?),
        None => None,
    };
    if connector_id.is_some_and(|connector| connector != request.connector_id) {
        return None;
    }
    let schedule_start = timestamp(root.get("scheduleStart")).ok()?;
    horizon(schedule_start, request.duration)?;
    let raw_schedule = root.get("chargingSchedule");
    if status == CompositeScheduleStatus16::Accepted
        && (schedule_start.is_none() || raw_schedule.is_none())
    {
        return None;
    }
    // The pinned native model remains the decoder; the raw guard closes permissive serde holes.
    let native = if let Ok(native) = GetCompositeScheduleResponse::deserialize(payload) {
        native
    } else {
        let decoder_payload = decoder_payload(payload)?;
        GetCompositeScheduleResponse::deserialize(decoder_payload.as_ref()).ok()?
    };
    let charging_schedule = match (raw_schedule, native.charging_schedule) {
        (None, None) => None,
        (Some(raw), Some(native)) => {
            let raw = object(
                raw,
                &[
                    "duration",
                    "startSchedule",
                    "chargingRateUnit",
                    "chargingSchedulePeriod",
                    "minChargingRate",
                ],
                &["chargingRateUnit", "chargingSchedulePeriod"],
            )?;
            let duration = native.duration;
            if duration.is_some_and(|duration| duration <= 0 || duration > request.duration) {
                return None;
            }
            let seconds = duration.unwrap_or(request.duration);
            let start_schedule = timestamp(raw.get("startSchedule")).ok()?;
            horizon(schedule_start, seconds)?;
            horizon(start_schedule, seconds)?;
            let unit = match raw["chargingRateUnit"].as_str()? {
                "A" => CompositeScheduleRateUnit16::A,
                "W" => CompositeScheduleRateUnit16::W,
                _ => return None,
            };
            if request
                .charging_rate_unit
                .is_some_and(|requested| requested != unit)
            {
                return None;
            }
            let raw_periods = raw["chargingSchedulePeriod"].as_array()?;
            if raw_periods.is_empty() || raw_periods.len() != native.charging_schedule_period.len()
            {
                return None;
            }
            let mut periods = Vec::with_capacity(raw_periods.len());
            let mut previous = None;
            for (raw, period) in raw_periods.iter().zip(native.charging_schedule_period) {
                let raw = object(
                    raw,
                    &["startPeriod", "limit", "numberPhases"],
                    &["startPeriod", "limit"],
                )?;
                if period.start_period < 0
                    || period.start_period >= seconds
                    || previous.map_or(period.start_period != 0, |previous| {
                        period.start_period <= previous
                    })
                    || period.number_phases.is_some_and(|phases| phases <= 0)
                {
                    return None;
                }
                let limit = ExactDecimal::new(period.limit.mantissa(), period.limit.scale());
                if exact_rate(&raw["limit"])? != limit {
                    return None;
                }
                previous = Some(period.start_period);
                periods.push(CompositeSchedulePeriod16 {
                    start_period: period.start_period,
                    limit,
                    number_phases: period.number_phases,
                });
            }
            let min_charging_rate = match (raw.get("minChargingRate"), native.min_charging_rate) {
                (None, None) => None,
                (Some(raw), Some(rate)) => {
                    let rate = ExactDecimal::new(rate.mantissa(), rate.scale());
                    if exact_rate(raw)? != rate {
                        return None;
                    }
                    Some(rate)
                }
                _ => return None,
            };
            Some(CompositeSchedule16 {
                duration,
                start_schedule,
                charging_rate_unit: unit,
                charging_schedule_period: periods,
                min_charging_rate,
            })
        }
        _ => return None,
    };
    Some(CompositeScheduleResult16 {
        request,
        status,
        connector_id,
        schedule_start,
        charging_schedule,
    })
}

// Original JSON numeric lexemes are checked before accepting the native Decimal's mantissa.
// Decimal::from_str may round at its precision boundary. Never normalize such loss into evidence.
fn exact_rate(value: &Value) -> Option<ExactDecimal> {
    let Value::Number(number) = value else {
        return None;
    };
    let lexeme = number.as_str();
    let unsigned = lexeme.strip_prefix('-').unwrap_or(lexeme);
    let (significand, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((significand, exponent)) => (significand, exponent.parse::<i64>().ok()?),
        None => (unsigned, 0),
    };
    let fraction = significand
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let trailing = significand
        .bytes()
        .rev()
        .filter(|byte| *byte != b'.')
        .take_while(|byte| *byte == b'0')
        .count();
    let digits = significand.bytes().filter(|byte| *byte != b'.');
    let digit_count = significand.len() - usize::from(significand.contains('.'));
    let mut coefficient = 0_i128;
    for digit in digits.take(digit_count - trailing) {
        coefficient = coefficient
            .checked_mul(10)?
            .checked_add(i128::from(digit - b'0'))?;
    }
    if coefficient == 0 {
        return Some(ExactDecimal::new(0, 0));
    }
    if lexeme.starts_with('-') {
        return None;
    }
    let scale = i64::try_from(fraction)
        .ok()?
        .checked_sub(i64::try_from(trailing).ok()?)?
        .checked_sub(exponent)?;
    if scale > 1 {
        return None;
    }
    if scale < 0 {
        let power = u32::try_from(scale.checked_neg()?).ok()?;
        coefficient = coefficient.checked_mul(10_i128.checked_pow(power)?)?;
    }
    // The native Decimal mantissa is unsigned 96-bit. Do not let its decoder round
    // a just-out-of-range tenth into a representable integer.
    if coefficient > (1_i128 << 96) - 1 {
        return None;
    }
    Some(ExactDecimal::new(
        coefficient,
        u32::try_from(scale.max(0)).ok()?,
    ))
}

// Normalize only decoder-incompatible rate lexemes, retaining the original payload
// for shape and exactness checks. Scientific notation may exceed the decoder's
// intermediate precision even when its final value fits the pinned Decimal.
fn decoder_payload(payload: &Value) -> Option<Cow<'_, Value>> {
    let Some(schedule) = payload.get("chargingSchedule") else {
        return Some(Cow::Borrowed(payload));
    };
    let periods = schedule.get("chargingSchedulePeriod")?.as_array()?;
    let mut normalized = Cow::Borrowed(payload);
    for (index, period) in periods.iter().enumerate() {
        if let Cow::Owned(value) = decoder_rate(period.get("limit")?)? {
            normalized.to_mut()["chargingSchedule"]["chargingSchedulePeriod"][index]["limit"] =
                value;
        }
    }
    if let Some(value) = schedule.get("minChargingRate")
        && let Cow::Owned(value) = decoder_rate(value)?
    {
        normalized.to_mut()["chargingSchedule"]["minChargingRate"] = value;
    }
    Some(normalized)
}

fn decoder_rate(value: &Value) -> Option<Cow<'_, Value>> {
    let number = value.as_number()?;
    let scientific = number.as_str().contains(['e', 'E']);
    let large_integer = number
        .as_str()
        .parse::<u128>()
        .is_ok_and(|integer| integer > u128::from(u64::MAX));
    if !scientific && !large_integer {
        return Some(Cow::Borrowed(value));
    }

    let rate = exact_rate(value)?;
    let mut lexeme = rate.to_string();
    // The pinned Decimal Value visitor has no u128 support. A decimal point
    // selects its lossless numeric-string/map branch, including normalized exponents.
    if rate.scale() == 0 && rate.coefficient() > i128::from(u64::MAX) {
        lexeme.push_str(".0");
    }
    Some(Cow::Owned(Value::Number(lexeme.parse().ok()?)))
}
