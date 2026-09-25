//! Strict OCA-wire validation and safe-value classification for OCPP 1.6 configuration.
use rust_ocpp::v1_6::messages::get_configuration::GetConfigurationRequest;
use serde_json::Value;
use std::collections::BTreeMap;
use uob_contracts::{
    CommandErrorCode, ConfigurationChangeReference, ConfigurationKey, ConfigurationResult,
    ConfigurationWriteStatus, ResourceRef, UtcTimestamp,
};

use super::configuration_values::LocalConfigurationValues;

pub(super) const GET_SCHEMA: &str = "urn:OCPP:1.6:2019:12:GetConfigurationRequest";
pub(super) const CHANGE_REFERENCE_SCHEMA: &str =
    uob_contracts::CONFIGURATION_CHANGE_REFERENCE_SCHEMA;

/// Bounded facts learned from validated replies on this socket; discarded on reconnect.
#[derive(Default)]
pub(super) struct SessionFacts {
    pub max_keys: Option<usize>,
    pub readonly: BTreeMap<String, bool>,
}

impl SessionFacts {
    pub fn learn(&mut self, result: &ConfigurationResult) {
        if let ConfigurationResult::Read {
            keys: Some(keys), ..
        } = result
        {
            for key in keys {
                if let Some(readonly) = self.readonly.get_mut(&key.key) {
                    *readonly |= key.readonly;
                } else if self.readonly.len() < 256 {
                    self.readonly.insert(key.key.clone(), key.readonly);
                }
                if key.key == "GetConfigurationMaxKeys"
                    && let Some(limit) = key
                        .value
                        .as_deref()
                        .and_then(|value| value.parse::<usize>().ok())
                    && (1..=256).contains(&limit)
                {
                    self.max_keys = Some(limit);
                }
            }
        }
    }
}
fn keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|field| object.contains_key(*field))
    })
}
fn valid_key(value: &str) -> bool {
    value.chars().count() <= 50
}

/// Accepts exactly the published `GetConfiguration` request shape (including empty arrays).
/// The pinned Rust model encodes the request, but its array min/max annotations do not match
/// the OCA schema; explicit validation below takes precedence.
pub(super) fn get_request(value: &Value, max_keys: usize) -> Result<Value, CommandErrorCode> {
    if !keys(value, &[]) && !keys(value, &["key"]) {
        return Err(CommandErrorCode::InvalidParameters);
    }
    if let Some(value) = value.get("key") {
        let list = value
            .as_array()
            .ok_or(CommandErrorCode::InvalidParameters)?;
        if list.len() > max_keys
            || list.len() > 256
            || !list.iter().all(|key| key.as_str().is_some_and(valid_key))
        {
            return Err(CommandErrorCode::InvalidParameters);
        }
    }
    let request = GetConfigurationRequest {
        key: value.get("key").map(|keys| {
            keys.as_array()
                .expect("validated array")
                .iter()
                .map(|key| key.as_str().expect("validated key").to_owned())
                .collect()
        }),
    };
    serde_json::to_value(request).map_err(|_| CommandErrorCode::InvalidParameters)
}

/// Stored write payload contains only a key and an independently random opaque reference.
pub(super) fn change_request(
    value: &Value,
    resource: &ResourceRef,
    provider: &LocalConfigurationValues,
    now: UtcTimestamp,
) -> Result<Value, CommandErrorCode> {
    if !keys(value, &["key", "valueReference"]) {
        return Err(CommandErrorCode::InvalidParameters);
    }
    let key = value["key"]
        .as_str()
        .ok_or(CommandErrorCode::InvalidParameters)?;
    let reference = value["valueReference"]
        .as_str()
        .ok_or(CommandErrorCode::InvalidParameters)?;
    if !ConfigurationChangeReference::valid_parts(key, reference) {
        return Err(CommandErrorCode::InvalidParameters);
    }
    if !provider.authorized(reference, resource, key, now) {
        return Err(CommandErrorCode::PolicyRejected);
    }
    Ok(value.clone())
}

/// Strict response validation happens before any fields are copied to durable evidence.
pub(super) fn read_response(payload: &Value, request: &Value) -> Option<ConfigurationResult> {
    let object = payload.as_object()?;
    if object
        .keys()
        .any(|key| key != "configurationKey" && key != "unknownKey")
    {
        return None;
    }
    let known = if let Some(value) = object.get("configurationKey") {
        Some(value.as_array()?)
    } else {
        None
    };
    let unknown = if let Some(value) = object.get("unknownKey") {
        Some(value.as_array()?)
    } else {
        None
    };
    if known.is_some_and(|list| list.len() > 256) || unknown.is_some_and(|list| list.len() > 256) {
        return None;
    }
    let keys = if let Some(list) = known {
        Some(parse_configuration_keys(list)?)
    } else {
        None
    };
    let unknown_keys = if let Some(list) = unknown {
        let mut result = Vec::with_capacity(list.len());
        for key in list {
            let key = key.as_str()?.to_owned();
            if !valid_key(&key) {
                return None;
            }
            result.push(key);
        }
        Some(result)
    } else {
        None
    };
    let requested_keys = if let Some(keys) = request.get("key") {
        Some(
            keys.as_array()?
                .iter()
                .map(|entry| entry.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
        )
    } else {
        None
    };
    Some(ConfigurationResult::Read {
        requested_keys,
        keys,
        unknown_keys,
    })
}

fn parse_configuration_keys(list: &[Value]) -> Option<Vec<ConfigurationKey>> {
    let mut result = Vec::with_capacity(list.len());
    for entry in list {
        let fields = entry.as_object()?;
        if fields.len() < 2
            || fields.len() > 3
            || !fields.contains_key("key")
            || !fields.contains_key("readonly")
            || fields
                .keys()
                .any(|key| key != "key" && key != "readonly" && key != "value")
        {
            return None;
        }
        let key = entry["key"].as_str()?.to_owned();
        if !valid_key(&key) {
            return None;
        }
        let readonly = entry["readonly"].as_bool()?;
        let value = if let Some(value) = fields.get("value") {
            Some(value.as_str()?)
        } else {
            None
        };
        if value.is_some_and(|text| text.chars().count() > 500) {
            return None;
        }
        let redacted = value.is_some_and(|text| !safe_value(&key, text));
        result.push(ConfigurationKey {
            key,
            readonly,
            value: if redacted {
                None
            } else {
                value.map(str::to_owned)
            },
            redacted,
        });
    }
    Some(result)
}

pub(super) fn write_response(payload: &Value, key: &str) -> Option<ConfigurationResult> {
    if !keys(payload, &["status"]) {
        return None;
    }
    let status = match payload["status"].as_str()? {
        "Accepted" => ConfigurationWriteStatus::Accepted,
        "Rejected" => ConfigurationWriteStatus::Rejected,
        "RebootRequired" => ConfigurationWriteStatus::RebootRequired,
        "NotSupported" => ConfigurationWriteStatus::NotSupported,
        _ => return None,
    };
    Some(ConfigurationResult::Write {
        key: key.to_owned(),
        status,
    })
}

/// Explicit OCPP Core keys with low-sensitivity scalar semantics; all other keys are opaque.
fn safe_value(key: &str, value: &str) -> bool {
    matches!(
        key,
        "HeartbeatInterval"
            | "GetConfigurationMaxKeys"
            | "NumberOfConnectors"
            | "MeterValueSampleInterval"
            | "ClockAlignedDataInterval"
            | "MinimumStatusDuration"
            | "ConnectionTimeOut"
            | "ResetRetries"
            | "WebSocketPingInterval"
    ) && !value.is_empty()
        && value.len() <= 10
        && value.as_bytes().iter().all(u8::is_ascii_digit)
        && value.parse::<u32>().is_ok()
}
