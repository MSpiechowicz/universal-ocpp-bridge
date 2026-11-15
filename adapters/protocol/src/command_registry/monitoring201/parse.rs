//! Strict conversion of native monitoring wire JSON into contract types. Callers validate the
//! pinned schema first; these checks add the native limits the schema cannot express. Every
//! function is shared by request validation, reply correspondence and report sanitizing, so the
//! three agree on what a representable value is.
use super::super::device_model201::{IdentityKey, identity_key};
use crate::v201::remote_control::device_model_values::{component, variable};
use serde::Deserialize;
use serde_json::Value;
use uob_contracts::{
    DeviceAttributeType201, DeviceComponent201, DeviceVariable201, ExactDecimal,
    MonitorSeverity201, MonitorType201, SetMonitor201,
};

/// Largest fractional scale accepted for a monitor value; bounds the canonical text a hostile
/// exponent could otherwise force the decimal to render.
const MAX_SCALE: i64 = 64;

/// Identity of one monitor on one variable: native case-insensitive names, type and severity.
pub(crate) type MonitorKey = (IdentityKey, u8, u8);

pub(crate) fn int32(value: &Value) -> Option<i32> {
    i32::try_from(value.as_i64()?).ok()
}

/// A present optional field that is not a valid native value.
pub(crate) struct Invalid;

pub(crate) fn optional_int32(value: Option<&Value>) -> Result<Option<i32>, Invalid> {
    match value {
        None => Ok(None),
        Some(value) => int32(value).map(Some).ok_or(Invalid),
    }
}

pub(crate) fn severity(value: &Value) -> Option<MonitorSeverity201> {
    MonitorSeverity201::new(u8::try_from(value.as_i64()?).ok()?).ok()
}

/// Replaces every control character with U+FFFD, keeping the length in characters so the schema
/// length limits still hold. For charger text the bridge only records, where a request or reply
/// would be refused by [`clean`] instead.
pub(crate) fn replace_controls(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                char::REPLACEMENT_CHARACTER
            } else {
                character
            }
        })
        .collect()
}

/// Native request and reply text never carries control characters, at any depth.
pub(crate) fn clean(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.chars().any(char::is_control),
        Value::Array(values) => values.iter().all(clean),
        Value::Object(fields) => fields.values().all(clean),
        _ => true,
    }
}

/// Converts a JSON number to an exact decimal from its original lexeme, never through binary
/// floating point. `None` when the value has no exact representation within the bounds.
pub(crate) fn exact_number(value: &Value) -> Option<ExactDecimal> {
    let lexeme = value.as_number()?.as_str();
    let (negative, unsigned) = match lexeme.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, lexeme),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (unsigned, 0),
    };
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{integer}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(ExactDecimal::new(0, 0));
    }

    let mut coefficient = digits.parse::<i128>().ok()?;
    if negative {
        coefficient = -coefficient;
    }
    let scale = i64::try_from(fraction.len()).ok()?.checked_sub(exponent)?;
    if scale > MAX_SCALE {
        return None;
    }
    if scale < 0 {
        let factor = 10_i128.checked_pow(u32::try_from(scale.checked_neg()?).ok()?)?;
        return Some(ExactDecimal::new(coefficient.checked_mul(factor)?, 0));
    }

    Some(ExactDecimal::new(coefficient, u32::try_from(scale).ok()?))
}

/// One `SetMonitoringData` element; `None` when any field is outside the native bounds.
pub(crate) fn set_monitor(item: &Value) -> Option<SetMonitor201> {
    let transaction = match item.get("transaction") {
        None => None,
        Some(flag) => Some(flag.as_bool()?),
    };

    Some(SetMonitor201 {
        id: optional_int32(item.get("id")).ok()?,
        transaction,
        value: exact_number(&item["value"])?,
        monitor_type: MonitorType201::deserialize(&item["type"]).ok()?,
        severity: severity(&item["severity"])?,
        component: component(&item["component"])?,
        variable: variable(&item["variable"])?,
    })
}

const fn type_rank(monitor_type: MonitorType201) -> u8 {
    match monitor_type {
        MonitorType201::UpperThreshold => 0,
        MonitorType201::LowerThreshold => 1,
        MonitorType201::Delta => 2,
        MonitorType201::Periodic => 3,
        MonitorType201::PeriodicClockAligned => 4,
    }
}

/// Case-folded identity of one monitor; the charger allows only one per variable, type and
/// severity (N04.FR.10).
pub(crate) fn monitor_key(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
    monitor_type: MonitorType201,
    severity: MonitorSeverity201,
) -> MonitorKey {
    (
        identity_key(component, variable, DeviceAttributeType201::Actual),
        type_rank(monitor_type),
        severity.get(),
    )
}
