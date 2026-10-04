use super::{LocalAuthorization201Handle, validation};
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
pub(super) fn reply(payload: &Value, local: &LocalAuthorization201Handle) -> Value {
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
            .map(|index| value(&model, index));
        let status = if !actual {
            "NotSupportedAttributeType"
        } else if !VARIABLES.iter().any(|(c, _, _)| same_name(component, c)) {
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
// is copied into reports, and no unimplemented monitoring/configuration is claimed.
pub(super) fn report(
    action: &str,
    payload: &Value,
    local: &LocalAuthorization201Handle,
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
        let mut characteristics = json!({"dataType":data_type,"supportsMonitoring":false});
        if index == 2 {
            characteristics["minLimit"] = json!(0);
            characteristics["maxLimit"] = json!(super::ENTRY_LIMIT);
        }
        data.push(json!({"component":{"name":component},"variable":{"name":variable},
            "variableAttribute":[{"type":"Actual","value":value(&model,index),"mutability":"ReadOnly"}],
            "variableCharacteristics":characteristics}));
    }
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
