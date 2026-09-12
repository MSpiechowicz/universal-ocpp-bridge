//! Enforce explicit resource-local operation constraints without guessing unknown parameters.
use serde_json::{Value, json};
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, SupportedOperation, TypedValue, ValueType,
};

pub(super) fn validate(
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
            ValueType::Boolean | ValueType::Decimal => false,
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
