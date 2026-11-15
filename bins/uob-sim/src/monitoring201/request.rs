//! Strict native request parsing against the pinned OCPP 2.0.1 schemas (use cases N02-N06).
use super::config::{
    ComponentId201, MonitorType201, MonitoringBase201, MonitoringCriterion201, VariableId201,
};
use super::model::{component_from_json, variable_from_json};
use crate::local_authorization201::validation::valid_native;
use serde_json::Value;
use std::sync::LazyLock;

macro_rules! schema {
    ($name:ident, $file:literal) => {
        static $name: LazyLock<Value> = LazyLock::new(|| {
            serde_json::from_str(include_str!(concat!(
                "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/",
                $file,
                ".json"
            )))
            .expect("pinned schema")
        });
    };
}

schema!(SET, "SetVariableMonitoringRequest");
schema!(CLEAR, "ClearVariableMonitoringRequest");
schema!(REPORT, "GetMonitoringReportRequest");
schema!(BASE, "SetMonitoringBaseRequest");
schema!(LEVEL, "SetMonitoringLevelRequest");

/// One `SetMonitoringDataType`, with the request's own component and variable kept for the echo
/// in the result (N04.FR.02).
pub(crate) struct SetItem {
    pub(crate) id: Option<i32>,
    pub(crate) transaction: bool,
    pub(crate) value: f64,
    pub(crate) monitor_type: MonitorType201,
    pub(crate) severity: i32,
    pub(crate) component: ComponentId201,
    pub(crate) variable: VariableId201,
    pub(crate) raw_component: Value,
    pub(crate) raw_variable: Value,
}

pub(crate) struct ReportRequest {
    pub(crate) request_id: i32,
    pub(crate) criteria: Option<Vec<MonitoringCriterion201>>,
    pub(crate) selectors: Option<Vec<(ComponentId201, Option<VariableId201>)>>,
}

fn strict(schema: &Value, payload: &Value) -> Result<(), &'static str> {
    // An explicit null is never an absent optional field.
    let null = payload
        .as_object()
        .is_some_and(|fields| fields.values().any(Value::is_null));
    if null || !valid_native(schema, payload) {
        return Err("FormationViolation");
    }
    Ok(())
}

/// `ItemsPerMessage` is an `OccurrenceConstraintViolation` and `BytesPerMessage` a
/// `FormatViolation` (N06.FR.06/07).
fn within(
    payload: &Value,
    items: usize,
    item_limit: u32,
    byte_limit: u32,
) -> Result<(), &'static str> {
    if items > usize::try_from(item_limit).unwrap_or(0) {
        return Err("OccurrenceConstraintViolation");
    }
    if payload.to_string().len() > usize::try_from(byte_limit).unwrap_or(0) {
        return Err("FormatViolation");
    }
    Ok(())
}

pub(crate) fn parse_set(
    payload: &Value,
    item_limit: u32,
    byte_limit: u32,
) -> Result<Vec<SetItem>, &'static str> {
    strict(&SET, payload)?;
    let data = payload["setMonitoringData"]
        .as_array()
        .ok_or("FormationViolation")?;
    within(payload, data.len(), item_limit, byte_limit)?;
    data.iter()
        .map(|item| {
            Ok(SetItem {
                id: item
                    .get("id")
                    .and_then(Value::as_i64)
                    .and_then(|id| i32::try_from(id).ok()),
                transaction: item
                    .get("transaction")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                value: item["value"].as_f64().ok_or("FormationViolation")?,
                monitor_type: item["type"]
                    .as_str()
                    .and_then(MonitorType201::parse)
                    .ok_or("FormationViolation")?,
                severity: item["severity"]
                    .as_i64()
                    .and_then(|severity| i32::try_from(severity).ok())
                    .ok_or("FormationViolation")?,
                component: component_from_json(&item["component"]),
                variable: variable_from_json(&item["variable"]),
                raw_component: item["component"].clone(),
                raw_variable: item["variable"].clone(),
            })
        })
        .collect()
}

pub(crate) fn parse_clear(
    payload: &Value,
    item_limit: u32,
    byte_limit: u32,
) -> Result<Vec<i32>, &'static str> {
    strict(&CLEAR, payload)?;
    let ids = payload["id"].as_array().ok_or("FormationViolation")?;
    within(payload, ids.len(), item_limit, byte_limit)?;
    ids.iter()
        .map(|id| {
            id.as_i64()
                .and_then(|id| i32::try_from(id).ok())
                .ok_or("FormationViolation")
        })
        .collect()
}

pub(crate) fn parse_report(payload: &Value) -> Result<ReportRequest, &'static str> {
    strict(&REPORT, payload)?;
    // The pinned schema allows each criterion once (maxItems 3), which the generic checker skips.
    if payload
        .get("monitoringCriteria")
        .and_then(Value::as_array)
        .is_some_and(|criteria| criteria.len() > 3)
    {
        return Err("FormationViolation");
    }
    let criteria = payload
        .get("monitoringCriteria")
        .and_then(Value::as_array)
        .map(|criteria| {
            criteria
                .iter()
                .filter_map(|criterion| criterion.as_str().and_then(MonitoringCriterion201::parse))
                .collect()
        });
    let selectors = payload
        .get("componentVariable")
        .and_then(Value::as_array)
        .map(|selectors| {
            selectors
                .iter()
                .map(|selector| {
                    (
                        component_from_json(&selector["component"]),
                        selector.get("variable").map(variable_from_json),
                    )
                })
                .collect()
        });
    Ok(ReportRequest {
        request_id: payload["requestId"]
            .as_i64()
            .and_then(|id| i32::try_from(id).ok())
            .ok_or("FormationViolation")?,
        criteria,
        selectors,
    })
}

pub(crate) fn parse_base(payload: &Value) -> Result<MonitoringBase201, &'static str> {
    strict(&BASE, payload)?;
    payload["monitoringBase"]
        .as_str()
        .and_then(MonitoringBase201::parse)
        .ok_or("FormationViolation")
}

pub(crate) fn parse_level(payload: &Value) -> Result<i32, &'static str> {
    strict(&LEVEL, payload)?;
    payload["severity"]
        .as_i64()
        .and_then(|severity| i32::try_from(severity).ok())
        .ok_or("FormationViolation")
}
