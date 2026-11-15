//! A small JSON-schema interpreter for the vocabulary of the pinned OCA 2.0.1 schemas, so every
//! frame the station emits is checked against the byte-exact schema, not its own model.
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

fn corpus(path: &str) -> Value {
    let path = format!(
        "{}/../../tests/ocpp-fixtures/corpus/{path}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|_| panic!("{path}"))).unwrap()
}

/// One corpus wire frame such as `monitoring-set-mixed`.
#[must_use]
pub fn corpus_frame(name: &str) -> Value {
    corpus(&format!("wire/2.0.1/{name}.json"))
}

/// Panics unless `value` satisfies the pinned schema, e.g. `NotifyEventRequest`.
pub fn assert_valid(schema: &str, value: &Value) {
    let root = corpus(&format!("schemas/2.0.1/{schema}.json"));
    if let Err(reason) = check(&root, &root, value, "$") {
        panic!("{schema}: {reason}: {value}");
    }
}

/// Whether `value` satisfies the pinned schema.
#[must_use]
pub fn is_valid(schema: &str, value: &Value) -> bool {
    let root = corpus(&format!("schemas/2.0.1/{schema}.json"));
    check(&root, &root, value, "$").is_ok()
}

fn check(root: &Value, schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = root
            .pointer(reference.trim_start_matches('#'))
            .ok_or("dangling $ref")?;
        return check(root, target, value, path);
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        return Err(format!("{path}: not in the enumeration"));
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => object(root, schema, value, path),
        Some("array") => array(root, schema, value, path),
        Some("string") => {
            let text = value.as_str().ok_or(format!("{path}: not a string"))?;
            let limit = schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            if text.chars().count() as u64 > limit {
                return Err(format!("{path}: longer than {limit}"));
            }
            if schema.get("format").and_then(Value::as_str) == Some("date-time")
                && OffsetDateTime::parse(text, &Rfc3339).is_err()
            {
                return Err(format!("{path}: not a date-time"));
            }
            Ok(())
        }
        Some("integer") if value.is_i64() || value.is_u64() => Ok(()),
        Some("number") if value.is_number() => Ok(()),
        Some("boolean") if value.is_boolean() => Ok(()),
        Some(kind) => Err(format!("{path}: not a {kind}")),
        None => Ok(()),
    }
}

fn object(root: &Value, schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let fields = value.as_object().ok_or(format!("{path}: not an object"))?;
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = name.as_str().unwrap();
        if !fields.contains_key(name) {
            return Err(format!("{path}: missing {name}"));
        }
    }
    for (name, field) in fields {
        match schema
            .get("properties")
            .and_then(|properties| properties.get(name))
        {
            Some(property) => check(root, property, field, &format!("{path}.{name}"))?,
            None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                return Err(format!("{path}: unexpected {name}"));
            }
            None => {}
        }
    }
    Ok(())
}

fn array(root: &Value, schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let items = value.as_array().ok_or(format!("{path}: not an array"))?;
    let minimum = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
    let maximum = schema
        .get("maxItems")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    if (items.len() as u64) < minimum || items.len() as u64 > maximum {
        return Err(format!("{path}: item count"));
    }
    for (index, item) in items.iter().enumerate() {
        check(root, &schema["items"], item, &format!("{path}[{index}]"))?;
    }
    Ok(())
}
