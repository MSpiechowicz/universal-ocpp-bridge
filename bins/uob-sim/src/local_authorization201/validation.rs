use serde_json::Value;
use std::sync::LazyLock;

const SEND: &str =
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SendLocalListRequest.json");
const EVENT: &str = include_str!(
    "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/TransactionEventRequest.json"
);
static SEND_SCHEMA: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(SEND).expect("pinned schema"));
static EVENT_SCHEMA: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(EVENT).expect("pinned schema"));
static VARIABLES_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetVariablesRequest.json"
    ))
    .expect("pinned schema")
});
static RESET_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ResetRequest.json"
    ))
    .expect("pinned schema")
});
static BASE_REPORT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetBaseReportRequest.json"
    ))
    .expect("pinned schema")
});
static REPORT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetReportRequest.json"
    ))
    .expect("pinned schema")
});

pub(super) fn valid_update(value: &Value) -> bool {
    let root = &*SEND_SCHEMA;
    valid(root, root, value, 0)
        && value
            .get("localAuthorizationList")
            .and_then(Value::as_array)
            .is_none_or(|entries| {
                entries.iter().all(|entry| {
                    valid_token(&entry["idToken"])
                        && (value["updateType"] != "Full" || entry.get("idTokenInfo").is_some())
                        && entry.get("idTokenInfo").is_none_or(valid_info)
                })
            })
}

pub(crate) fn valid_info(value: &Value) -> bool {
    let root = &*SEND_SCHEMA;
    valid(root, &root["definitions"]["IdTokenInfoType"], value, 0)
        && value.get("evseId").is_none_or(|value| {
            value.as_array().is_some_and(|ids| {
                ids.iter().all(|id| {
                    id.as_i64()
                        .is_some_and(|n| n > 0 && i32::try_from(n).is_ok())
                })
            })
        })
        && value
            .get("chargingPriority")
            .is_none_or(|v| v.as_i64().is_some_and(|n| (-9..=9).contains(&n)))
        && value.get("language2").is_none_or(|v| {
            value
                .get("language1")
                .and_then(Value::as_str)
                .zip(v.as_str())
                .is_some_and(|(first, second)| !first.eq_ignore_ascii_case(second))
        })
        && value.get("groupIdToken").is_none_or(valid_token)
        && ["language1", "language2"].iter().all(|key| {
            value
                .get(key)
                .is_none_or(|v| v.as_str().is_some_and(valid_language))
        })
        && value
            .get("personalMessage")
            .and_then(|v| v.get("language"))
            .is_none_or(|v| v.as_str().is_some_and(valid_language))
}

pub(crate) fn valid_token(value: &Value) -> bool {
    let root = &*SEND_SCHEMA;
    valid(root, &root["definitions"]["IdTokenType"], value, 0)
        && value["idToken"].as_str().is_some_and(valid_identifier)
        && (value["type"] != "NoAuthorization" || value["idToken"] == "")
        && value
            .get("additionalInfo")
            .and_then(Value::as_array)
            .is_none_or(|info| {
                info.iter().all(|item| {
                    item["additionalIdToken"]
                        .as_str()
                        .is_some_and(valid_identifier)
                })
            })
}

/// Validates one complete native request against its own pinned OCA document.
pub(crate) fn valid_native(root: &Value, value: &Value) -> bool {
    valid(root, root, value, 0)
}

pub(super) fn valid_event(value: &Value) -> bool {
    let root = &*EVENT_SCHEMA;
    valid(root, root, value, 0) && value.get("idToken").is_none_or(valid_token)
}

// Interpreter for the vocabulary used by these pinned native schemas. Bounds
// also apply to arbitrary vendor customData, which the schemas leave extensible.
fn valid(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return reference
            .strip_prefix('#')
            .and_then(|path| root.pointer(path))
            .is_some_and(|target| valid(root, target, value, depth + 1));
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|choices| !choices.contains(value))
    {
        return false;
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(fields) = value.as_object() else {
                return false;
            };
            if schema
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|names| {
                    names
                        .iter()
                        .any(|name| name.as_str().is_none_or(|name| !fields.contains_key(name)))
                })
            {
                return false;
            }
            fields.iter().all(|(name, value)| {
                if let Some(property) = schema.get("properties").and_then(|p| p.get(name)) {
                    valid(root, property, value, depth + 1)
                } else {
                    schema.get("additionalProperties") != Some(&Value::Bool(false))
                        && bounded_json(value, depth + 1)
                }
            })
        }
        Some("array") => {
            let Some(values) = value.as_array() else {
                return false;
            };
            size_bound(schema, "minItems", 0).is_some_and(|minimum| values.len() >= minimum)
                && values.len() <= 256
                && values
                    .iter()
                    .all(|value| valid(root, &schema["items"], value, depth + 1))
        }
        Some("string") => value.as_str().is_some_and(|text| {
            size_bound(schema, "maxLength", 65_536)
                .is_some_and(|maximum| text.chars().count() <= maximum)
                && (schema.get("format").and_then(Value::as_str) != Some("date-time")
                    || super::native::valid_date(text))
        }),
        Some("integer") => value.as_i64().is_some_and(|n| i32::try_from(n).is_ok()),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        None => bounded_json(value, depth + 1),
        _ => false,
    }
}
fn size_bound(schema: &Value, key: &str, default: u64) -> Option<usize> {
    usize::try_from(schema.get(key).and_then(Value::as_u64).unwrap_or(default)).ok()
}
fn bounded_json(value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match value {
        Value::Array(values) => {
            values.len() <= 256 && values.iter().all(|v| bounded_json(v, depth + 1))
        }
        Value::Object(fields) => {
            fields.len() <= 256 && fields.values().all(|v| bounded_json(v, depth + 1))
        }
        _ => true,
    }
}

fn valid_identifier(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"*-_=:+|@.".contains(&byte))
}

// RFC 5646 syntax, including the short grandfathered and private-use tags
// that fit the native eight-character language bound. No locale normalization.
fn valid_language(tag: &str) -> bool {
    if tag.is_empty() || tag.len() > 8 || !tag.is_ascii() {
        return false;
    }
    let lower = tag.to_ascii_lowercase();
    if [
        "i-ami", "i-bnn", "i-hak", "i-lux", "i-navajo", "i-pwn", "i-tao", "i-tay", "i-tsu",
        "no-bok", "no-nyn", "zh-min",
    ]
    .contains(&lower.as_str())
    {
        return true;
    }
    let parts: Vec<&str> = tag.split('-').collect();
    let alpha = |part: &str| part.bytes().all(|byte| byte.is_ascii_alphabetic());
    let alnum = |part: &str| part.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if parts[0].eq_ignore_ascii_case("x") {
        return parts.len() > 1
            && parts[1..]
                .iter()
                .all(|part| (1..=8).contains(&part.len()) && alnum(part));
    }
    if !(2..=8).contains(&parts[0].len()) || !alpha(parts[0]) {
        return false;
    }
    let mut index = 1;
    let mut extlangs = 0;
    if parts[0].len() <= 3 {
        while index < parts.len() && parts[index].len() == 3 && alpha(parts[index]) && extlangs < 3
        {
            index += 1;
            extlangs += 1;
        }
    }
    if index < parts.len() && parts[index].len() == 4 && alpha(parts[index]) {
        index += 1;
    }
    if index < parts.len()
        && ((parts[index].len() == 2 && alpha(parts[index]))
            || (parts[index].len() == 3 && parts[index].bytes().all(|b| b.is_ascii_digit())))
    {
        index += 1;
    }
    while index < parts.len()
        && ((5..=8).contains(&parts[index].len())
            || (parts[index].len() == 4 && parts[index].as_bytes()[0].is_ascii_digit()))
    {
        if !alnum(parts[index]) {
            return false;
        }
        index += 1;
    }
    while index < parts.len() {
        let singleton = parts[index];
        if singleton.len() != 1 || !alnum(singleton) {
            return false;
        }
        index += 1;
        let start = index;
        let private = singleton.eq_ignore_ascii_case("x");
        while index < parts.len()
            && (if private { 1 } else { 2 }..=8).contains(&parts[index].len())
            && alnum(parts[index])
        {
            index += 1;
        }
        if index == start {
            return false;
        }
        if private {
            return index == parts.len();
        }
    }
    true
}

pub(super) fn valid_variables(value: &Value) -> bool {
    valid(&VARIABLES_SCHEMA, &VARIABLES_SCHEMA, value, 0)
}
pub(super) fn valid_reset(value: &Value) -> bool {
    valid(&RESET_SCHEMA, &RESET_SCHEMA, value, 0)
}
pub(super) fn valid_report(action: &str, value: &Value) -> bool {
    let schema = if action == "GetBaseReport" {
        &*BASE_REPORT_SCHEMA
    } else {
        &*REPORT_SCHEMA
    };
    valid(schema, schema, value, 0)
}
