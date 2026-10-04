use crate::command_registry::device_model201::{
    contained, identity_key, valid_component, valid_schema,
};
use caseless::Caseless;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use uob_contracts::{
    DeviceAttributeType201, DeviceCharacteristics201, DeviceComponent201, DeviceDataType201,
    DeviceEvse201, DeviceModelQuery201, DeviceMutability201, DeviceReportAttribute201,
    DeviceReportItem201, DeviceValue201, DeviceVariable201, DeviceVariableQuery201,
    DeviceVariableResult201, DeviceVariableStatus201, NativeProtocolReference, ResourceRef,
};

pub(super) fn component(value: &Value) -> Option<DeviceComponent201> {
    let evse = if let Some(evse) = value.get("evse") {
        let id = i32::try_from(evse["id"].as_i64()?).ok()?;
        let connector_id = if let Some(id) = evse.get("connectorId") {
            Some(i32::try_from(id.as_i64()?).ok()?)
        } else {
            None
        };
        Some(DeviceEvse201 { id, connector_id })
    } else {
        None
    };
    let result = DeviceComponent201 {
        name: value["name"].as_str()?.to_owned(),
        instance: value
            .get("instance")
            .and_then(Value::as_str)
            .map(str::to_owned),
        evse,
    };
    valid_component(&result).then_some(result)
}
pub(super) fn variable(value: &Value) -> Option<DeviceVariable201> {
    Some(DeviceVariable201 {
        name: value["name"].as_str()?.to_owned(),
        instance: value
            .get("instance")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
fn named(value: &str, lowercase: &str) -> bool {
    value.chars().default_case_fold().eq(lowercase.chars())
}
/// Only explicitly queried native station request-limit identities disclose numeric values.
#[must_use]
pub fn limit_identity(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
) -> Option<(usize, bool)> {
    if !named(&component.name, "devicedatactrlr")
        || component.instance.is_some()
        || component.evse.is_some()
    {
        return None;
    }
    let action = match variable.instance.as_deref()? {
        name if named(name, "getvariables") => 0,
        name if named(name, "getreport") => 1,
        name if named(name, "setvariables") => 2,
        _ => return None,
    };
    if named(&variable.name, "itemspermessage") {
        Some((action, false))
    } else if named(&variable.name, "bytespermessage") {
        Some((action, true))
    } else {
        None
    }
}
fn disclose(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
    attribute: DeviceAttributeType201,
    value: Option<&str>,
    write_only: bool,
) -> DeviceValue201 {
    let safe = !write_only
        && attribute == DeviceAttributeType201::Actual
        && ((super::phase_capability::identity(component, variable).is_some()
            && matches!(value, Some("true" | "false")))
            || (limit_identity(component, variable).is_some()
                && value.is_some_and(|value| {
                    !value.is_empty()
                        && value.len() <= 10
                        && value.bytes().all(|b| b.is_ascii_digit())
                        && value
                            .parse::<u32>()
                            .is_ok_and(|n| n > 0 && i32::try_from(n).is_ok())
                }))
            || value.is_some_and(|value| {
                super::local_authorization_limits::safe_value(component, variable, value)
            }));
    DeviceValue201 {
        present: value.is_some(),
        redacted: value.is_some() && !safe,
        empty: value == Some(""),
        value: safe.then(|| value.expect("safe value").to_owned()),
    }
}
pub fn variables(
    query: &[DeviceVariableQuery201],
    payload: &Value,
    resource: &ResourceRef,
) -> Option<Vec<DeviceVariableResult201>> {
    if !valid_schema(1, payload) {
        return None;
    }
    let entries = payload["getVariableResult"].as_array()?;
    if entries.len() != query.len() {
        return None;
    }
    let mut matched = vec![false; query.len()];
    let mut results = Vec::with_capacity(entries.len());
    let identities = query
        .iter()
        .enumerate()
        .map(|(index, request)| {
            (
                identity_key(
                    &request.component,
                    &request.variable,
                    request.attribute_type.unwrap_or_default(),
                ),
                index,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for entry in entries {
        let component = component(&entry["component"])?;
        let variable = variable(&entry["variable"])?;
        if !contained(resource, &component) {
            return None;
        }
        let attribute = entry
            .get("attributeType")
            .map(DeviceAttributeType201::deserialize)
            .transpose()
            .ok()?
            .unwrap_or_default();
        let index = *identities.get(&identity_key(&component, &variable, attribute))?;
        if matched[index] {
            return None;
        }
        matched[index] = true;
        let status = DeviceVariableStatus201::deserialize(&entry["attributeStatus"]).ok()?;
        let value = entry.get("attributeValue").and_then(Value::as_str);
        // B06.FR13 allows Accepted with an explicitly empty value, but not no value.
        if (status == DeviceVariableStatus201::Accepted) != value.is_some() {
            return None;
        }
        results.push(DeviceVariableResult201 {
            value: disclose(&component, &variable, attribute, value, false),
            component,
            variable,
            attribute_type: attribute,
            status,
        });
    }
    Some(results)
}
pub fn report_items(payload: &Value, resource: &ResourceRef) -> Option<Vec<DeviceReportItem201>> {
    let entries = if let Some(entries) = payload.get("reportData") {
        entries.as_array()?.as_slice()
    } else {
        &[]
    };
    if entries.len() > 4096 {
        return None;
    }
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let component = component(&entry["component"])?;
        let variable = variable(&entry["variable"])?;
        if !contained(resource, &component) {
            return None;
        }
        let mut attributes = Vec::with_capacity(4);
        for native in entry["variableAttribute"].as_array()? {
            let attribute_type = native
                .get("type")
                .map(DeviceAttributeType201::deserialize)
                .transpose()
                .ok()?
                .unwrap_or_default();
            if attributes
                .iter()
                .any(|attribute: &DeviceReportAttribute201| {
                    attribute.attribute_type == attribute_type
                })
            {
                return None;
            }
            let mutability = native
                .get("mutability")
                .map(DeviceMutability201::deserialize)
                .transpose()
                .ok()?;
            let value = native.get("value").and_then(Value::as_str);
            // B07.FR.11 includes supported-but-unset attributes; absence is native evidence.
            attributes.push(DeviceReportAttribute201 {
                attribute_type,
                value: disclose(
                    &component,
                    &variable,
                    attribute_type,
                    value,
                    mutability == Some(DeviceMutability201::WriteOnly),
                ),
                mutability,
                persistent: native.get("persistent").and_then(Value::as_bool),
                constant: native.get("constant").and_then(Value::as_bool),
            });
        }
        let characteristics = if let Some(native) = entry.get("variableCharacteristics") {
            Some(DeviceCharacteristics201 {
                unit: native
                    .get("unit")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                data_type: DeviceDataType201::deserialize(&native["dataType"]).ok()?,
                min_limit: native.get("minLimit").and_then(Value::as_number).cloned(),
                max_limit: native.get("maxLimit").and_then(Value::as_number).cloned(),
                supports_monitoring: native["supportsMonitoring"].as_bool()?,
            })
        } else {
            None
        };
        result.push(DeviceReportItem201 {
            component,
            variable,
            attributes,
            characteristics,
        });
    }
    Some(result)
}

/// Generation-scoped learned station limits. No probing, splitting or retries.
#[derive(Default)]
pub(crate) struct LearnedLimits {
    pub values: [[Option<usize>; 2]; 3],
    pub local:
        std::sync::Arc<std::sync::Mutex<super::local_authorization_limits::LocalListLimits201>>,
}
impl LearnedLimits {
    pub fn learn(
        &mut self,
        component: &DeviceComponent201,
        variable: &DeviceVariable201,
        attribute: DeviceAttributeType201,
        value: &DeviceValue201,
    ) {
        if attribute != DeviceAttributeType201::Actual {
            return;
        }
        if let Ok(mut local) = self.local.lock() {
            local.learn(component, variable, attribute, value);
        }
        if let Some((action, bytes)) = limit_identity(component, variable)
            && let Some(number) = value
                .value
                .as_deref()
                .and_then(|value| value.parse::<usize>().ok())
        {
            self.values[action][usize::from(bytes)] = Some(number);
        }
    }
    pub fn allows(&self, query: &DeviceModelQuery201, bytes: usize) -> bool {
        let (action, items) = match query {
            DeviceModelQuery201::GetVariables { entries } => (0, entries.len()),
            DeviceModelQuery201::GetReport { selectors, .. } => (1, selectors.len()),
            DeviceModelQuery201::GetBaseReport { .. } => return bytes <= 256 * 1024,
        };
        if bytes > 256 * 1024 || items > 4096 {
            return false;
        }
        let [item_limit, byte_limit] = self.values[action];
        if items > 1 && (item_limit.is_none() || byte_limit.is_none()) {
            return false;
        }
        item_limit.is_none_or(|limit| items <= limit)
            && byte_limit.is_none_or(|limit| bytes <= limit)
    }
    pub fn allows_variables(&self, items: usize, bytes: usize) -> bool {
        if items == 0 || items > 4096 || bytes > 256 * 1024 {
            return false;
        }
        let [item_limit, byte_limit] = self.values[2];
        if items > 1 && (item_limit.is_none() || byte_limit.is_none()) {
            return false;
        }
        item_limit.is_none_or(|limit| items <= limit)
            && byte_limit.is_none_or(|limit| bytes <= limit)
    }
}

pub(crate) fn report_scope_matches(payload: &Value, resource: &ResourceRef) -> bool {
    if resource.resource.is_none() && resource.native_protocol_reference.is_none() {
        return true;
    }
    let Some(NativeProtocolReference::Ocpp201 {
        evse_id,
        connector_id,
    }) = &resource.native_protocol_reference
    else {
        return false;
    };
    payload
        .get("reportData")
        .and_then(Value::as_array)
        .is_none_or(|items| {
            items.iter().all(|item| {
                let evse = &item["component"]["evse"];
                evse["id"].as_i64() == Some(i64::from(*evse_id))
                    && connector_id
                        .is_none_or(|id| evse["connectorId"].as_i64() == Some(i64::from(id)))
            })
        })
}
