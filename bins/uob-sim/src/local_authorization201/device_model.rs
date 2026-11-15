use super::{LocalAuthorization201Handle, validation};
use crate::monitoring201::{DeviceVariable201, knows_component};
use caseless::Caseless;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const VARIABLES: [(&str, &str, &str); 7] = [
    ("LocalAuthListCtrlr", "ItemsPerMessage", "integer"),
    ("LocalAuthListCtrlr", "BytesPerMessage", "integer"),
    ("LocalAuthListCtrlr", "Entries", "integer"),
    ("LocalAuthListCtrlr", "Enabled", "boolean"),
    ("LocalAuthListCtrlr", "Available", "boolean"),
    ("LocalAuthListCtrlr", "SupportsExpiryDateTime", "boolean"),
    ("AuthCacheCtrlr", "Enabled", "boolean"),
];
fn root_identity(component: &Value, variable: Option<&Value>) -> bool {
    component.get("instance").is_none()
        && component.get("evse").is_none()
        && variable.is_none_or(|v| v.get("instance").is_none())
}
fn same_name(value: &Value, name: &str) -> bool {
    value["name"].as_str().is_some_and(|v| {
        v.chars()
            .default_case_fold()
            .eq(name.chars().default_case_fold())
    })
}
fn value(local: &super::Model, index: usize) -> String {
    match index {
        0 => super::ENTRY_LIMIT.to_string(),
        1 => "65536".to_owned(),
        2 => local.state.list.len().to_string(),
        3 | 4 => local.list_enabled.to_string(),
        5 => "true".to_owned(),
        6 => local.cache_enabled.to_string(),
        _ => unreachable!("fixed controller inventory"),
    }
}
/// `GetVariables`. The monitoring model adds its own variables to the fixed controller inventory.
pub(super) fn reply(
    payload: &Value,
    local: &LocalAuthorization201Handle,
    monitored: &[DeviceVariable201],
) -> Value {
    if !validation::valid_variables(payload) {
        return json!({"callError":"FormationViolation"});
    }
    let model = local.0.lock();
    let mut results = Vec::new();
    for query in payload["getVariableData"]
        .as_array()
        .expect("validated native query")
    {
        let component = &query["component"];
        let variable = &query["variable"];
        let actual = query.get("attributeType").is_none_or(|v| v == "Actual");
        let index = VARIABLES
            .iter()
            .position(|(c, v, _)| same_name(component, c) && same_name(variable, v));
        let attribute = index
            .filter(|_| actual && root_identity(component, Some(variable)) && !model.unavailable)
            .map(|index| value(&model, index))
            .or_else(|| {
                monitored
                    .iter()
                    .find(|entry| actual && entry.matches_query(component, variable))
                    .map(|entry| entry.value().to_owned())
            });
        let known_component = VARIABLES.iter().any(|(c, _, _)| same_name(component, c))
            || knows_component(monitored, component);
        let status = if !actual {
            "NotSupportedAttributeType"
        } else if !known_component {
            "UnknownComponent"
        } else if attribute.is_some() {
            "Accepted"
        } else {
            "UnknownVariable"
        };
        let mut result =
            json!({"attributeStatus":status,"component":component,"variable":variable});
        if let Some(attribute_type) = query.get("attributeType") {
            result["attributeType"] = attribute_type.clone();
        }
        if let Some(attribute) = attribute {
            result["attributeValue"] = json!(attribute);
        }
        results.push(result);
    }
    json!({"getVariableResult":results})
}

// One finite, read-only controller inventory. No identifier or private metadata
// is copied into reports. Monitoring facts come only from a configured monitoring model.
pub(super) fn report(
    action: &str,
    payload: &Value,
    local: &LocalAuthorization201Handle,
    monitored: &[DeviceVariable201],
) -> (Value, Option<Value>) {
    if !validation::valid_report(action, payload) {
        return (json!({"callError":"FormationViolation"}), None);
    }
    if payload
        .get("componentVariable")
        .and_then(Value::as_array)
        .is_some_and(|v| v.len() > 256)
    {
        return (json!({"status":"Rejected"}), None);
    }
    let model = local.0.lock();
    if model.unavailable {
        return (json!({"status":"Rejected"}), None);
    }
    // Active and Problem have no implemented controller facts. Do not invent them.
    if payload
        .get("componentCriteria")
        .and_then(Value::as_array)
        .is_some_and(|criteria| criteria.iter().any(|v| v != "Available" && v != "Enabled"))
    {
        return (json!({"status":"NotSupported"}), None);
    }
    let mut data = Vec::with_capacity(VARIABLES.len());
    for (index, (component, variable, data_type)) in VARIABLES.iter().enumerate() {
        if action == "GetBaseReport" {
            if payload["reportBase"] == "ConfigurationInventory" {
                continue;
            }
            if payload["reportBase"] == "SummaryInventory" && !matches!(index, 3 | 4 | 6) {
                continue;
            }
        }
        if payload.get("componentCriteria").is_some()
            && !(if *component == "LocalAuthListCtrlr" {
                model.list_enabled
            } else {
                model.cache_enabled
            })
        {
            continue;
        }
        if let Some(selectors) = payload.get("componentVariable").and_then(Value::as_array)
            && !selectors.iter().any(|selector| {
                root_identity(&selector["component"], selector.get("variable"))
                    && same_name(&selector["component"], component)
                    && selector
                        .get("variable")
                        .is_none_or(|v| same_name(v, variable))
            })
        {
            continue;
        }
        // The monitoring model decides which variables can be monitored.
        let supports_monitoring = monitored.iter().any(|entry| {
            entry.supports_monitoring()
                && entry.matches_query(&json!({"name": component}), &json!({"name": variable}))
        });
        let mut characteristics =
            json!({"dataType":data_type,"supportsMonitoring":supports_monitoring});
        if index == 2 {
            characteristics["minLimit"] = json!(0);
            characteristics["maxLimit"] = json!(super::ENTRY_LIMIT);
        }
        data.push(json!({"component":{"name":component},"variable":{"name":variable},
            "variableAttribute":[{"type":"Actual","value":value(&model,index),"mutability":"ReadOnly"}],
            "variableCharacteristics":characteristics}));
    }
    data.extend(
        monitored
            .iter()
            .filter(|entry| monitored_selected(action, payload, entry))
            .map(DeviceVariable201::report_json),
    );
    if data.is_empty() {
        return (json!({"status":"EmptyResultSet"}), None);
    }
    let generated = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("valid seconds")
        .format(&Rfc3339)
        .expect("UTC timestamp");
    (
        json!({"status":"Accepted"}),
        Some(
            json!({"requestId":payload["requestId"],"generatedAt":generated,
        "seqNo":0,"tbc":false,"reportData":data}),
        ),
    )
}

/// Whether a report includes one monitoring-model variable.
fn monitored_selected(action: &str, payload: &Value, entry: &DeviceVariable201) -> bool {
    if action == "GetBaseReport" {
        // Everything monitoring owns is read-only configuration.
        if payload["reportBase"] == "ConfigurationInventory" {
            return false;
        }
        if payload["reportBase"] == "SummaryInventory" && !entry.summary() {
            return false;
        }
    }
    // Only the controller is known to be available and enabled.
    if payload.get("componentCriteria").is_some() && !entry.controller() {
        return false;
    }
    payload
        .get("componentVariable")
        .and_then(Value::as_array)
        .is_none_or(|selectors| {
            selectors
                .iter()
                .any(|selector| entry.matches_selector(selector))
        })
}
