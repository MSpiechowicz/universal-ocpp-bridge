//! Enforce explicit resource-local operation constraints without guessing unknown parameters.
use serde_json::{Value, json};
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, SupportedOperation, TypedValue, ValueType,
};

/// Converts only lossless OCPP A/W quantities representable to the mandated tenth.
/// No voltage or phase inference is made from a different unit.
pub(crate) fn profile_quantity(
    limit: &uob_contracts::ChargingLimit,
) -> Result<(&'static str, serde_json::Number), CommandErrorCode> {
    use uob_contracts::EngineeringUnit;
    let invalid = CommandErrorCode::InvalidParameters;
    if limit
        .phases
        .is_some_and(|phases| !(1..=3).contains(&phases))
    {
        return Err(invalid);
    }
    let (unit, exponent) = match limit.unit {
        EngineeringUnit::Ampere => ("A", 0),
        EngineeringUnit::Milliampere => ("A", -3),
        EngineeringUnit::Watt => ("W", 0),
        EngineeringUnit::Kilowatt => ("W", 3),
        _ => return Err(invalid),
    };
    let quantity = limit
        .value
        .checked_scale_by_power_of_ten(exponent)
        .map_err(|_| invalid)?;
    // Both protocol editions allow at most one fractional digit in a schedule limit.
    // Keep the bound within exact JSON number round trips, with no silent float conversion.
    if quantity.coefficient() <= 0 || quantity.scale() > 1 || quantity.coefficient() > 10_000_000 {
        return Err(invalid);
    }
    let decimal = quantity.to_string();
    let number: serde_json::Number = decimal.parse().map_err(|_| invalid)?;
    if number.to_string() != decimal {
        return Err(invalid);
    }
    Ok((unit, number))
}

pub(crate) fn validate(
    command: &Command<Value>,
    descriptor: &SupportedOperation,
) -> Result<(), CommandErrorCode> {
    for parameter in &descriptor.parameters {
        let value = match (&command.operation, parameter.name.as_str()) {
            (
                CommandOperation::Start {
                    authorization_reference,
                },
                "authorization_reference",
            ) => authorization_reference.as_ref().map(|value| json!(value)),
            (CommandOperation::Stop { transaction_id }, "transaction_id") => {
                Some(json!(transaction_id))
            }
            (CommandOperation::SetChargingLimit(limit), "phases") => limit.phases.map(|v| json!(v)),
            (CommandOperation::SetChargingLimit(limit), "current_amps") => Some(json!(
                limit
                    .unit
                    .convert(limit.value, uob_contracts::EngineeringUnit::Ampere)
                    .map_err(|_| CommandErrorCode::InvalidParameters)?
            )),
            (CommandOperation::SetChargingLimit(limit), "power_watts") => Some(json!(
                limit
                    .unit
                    .convert(limit.value, uob_contracts::EngineeringUnit::Watt)
                    .map_err(|_| CommandErrorCode::InvalidParameters)?
            )),
            (CommandOperation::Ocpp(operation), name) => operation.payload.get(name).cloned(),
            _ => None,
        };
        let Some(value) = value else {
            if parameter.required {
                return Err(CommandErrorCode::InvalidParameters);
            }
            continue;
        };
        let valid_type = match parameter.value_type {
            ValueType::Text | ValueType::NamedEnum => value.is_string(),
            ValueType::UnsignedInteger => value.as_u64().is_some(),
            ValueType::SignedInteger => value.as_i64().is_some(),
            ValueType::Decimal => value
                .as_str()
                .is_some_and(|s| s.parse::<uob_contracts::ExactDecimal>().is_ok()),
            ValueType::Boolean => false,
        };
        let constraints = &parameter.constraints;
        if !valid_type
            || (!constraints.enum_values.is_empty()
                && !constraints
                    .enum_values
                    .iter()
                    .any(|allowed| value.as_str() == Some(allowed.as_str())))
            || constraints
                .minimum
                .as_ref()
                .is_some_and(|minimum| !within(&value, minimum, true))
            || constraints
                .maximum
                .as_ref()
                .is_some_and(|maximum| !within(&value, maximum, false))
        {
            return Err(CommandErrorCode::InvalidParameters);
        }
    }
    Ok(())
}
fn within(value: &Value, bound: &TypedValue, minimum: bool) -> bool {
    let ordering = match bound {
        TypedValue::UnsignedInteger(bound) => value.as_u64().map(|value| value.cmp(bound)),
        TypedValue::SignedInteger(bound) => value.as_i64().map(|value| value.cmp(bound)),
        TypedValue::Decimal(bound) => decimal_order(value, *bound),
        _ => None,
    };
    ordering.is_some_and(|order| {
        if minimum {
            !order.is_lt()
        } else {
            !order.is_gt()
        }
    })
}

fn decimal_order(value: &Value, bound: uob_contracts::ExactDecimal) -> Option<std::cmp::Ordering> {
    let value = value
        .as_str()?
        .parse::<uob_contracts::ExactDecimal>()
        .ok()?;
    let scale = value.scale().max(bound.scale());
    let value_factor = 10_i128.checked_pow(scale - value.scale())?;
    let bound_factor = 10_i128.checked_pow(scale - bound.scale())?;
    Some(
        value
            .coefficient()
            .checked_mul(value_factor)?
            .cmp(&bound.coefficient().checked_mul(bound_factor)?),
    )
}
