//! OCPP 2.0.1 variable monitoring (N02 to N06) on the pinned native schemas. The station root
//! is the only target: monitors belong to the charger's own device model.
mod descriptors;
pub(crate) mod parse;

pub(crate) use descriptors::descriptors;

use super::device_model201::opaque;
use super::firmware201::station_scope;
use parse::{clean, int32, monitor_key, set_monitor, severity};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, sync::LazyLock};
use uob_contracts::{
    CommandErrorCode, DeviceSelector201, MonitorType201, MonitoringBase201, MonitoringCriterion201,
    MonitoringQuery201, PrivilegedOcppOperation, ProtocolEdition, ResourceRef, SetMonitor201,
};

pub const ACTIONS: [&str; 5] = [
    "SetVariableMonitoring",
    "ClearVariableMonitoring",
    "GetMonitoringReport",
    "SetMonitoringBase",
    "SetMonitoringLevel",
];

/// Request then response schema per privileged action, in `ACTIONS` order, followed by the two
/// charger-initiated actions.
const SCHEMAS: [&str; 14] = [
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetVariableMonitoringRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetVariableMonitoringResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearVariableMonitoringRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ClearVariableMonitoringResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetMonitoringReportRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetMonitoringReportResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetMonitoringBaseRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetMonitoringBaseResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetMonitoringLevelRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetMonitoringLevelResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyMonitoringReportRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyMonitoringReportResponse.json"
    ),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyEventRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyEventResponse.json"),
];

/// Schema index of the request of `ACTIONS[index]`; the response follows it.
pub(crate) const fn request_schema(index: usize) -> usize {
    index * 2
}

pub(crate) const fn response_schema(index: usize) -> usize {
    index * 2 + 1
}

pub(crate) const NOTIFY_REPORT_SCHEMA: usize = 10;
pub(crate) const NOTIFY_EVENT_SCHEMA: usize = 12;

/// Upper bound on list entries the bridge ever sends in one request, before any learned limit.
const MAXIMUM_ITEMS: usize = 4096;

pub(crate) fn valid_schema(index: usize, payload: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let schema: Value = serde_json::from_str(source).expect("pinned schema JSON");
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });

    VALIDATORS[index].is_valid(payload)
}

/// Whether an operation belongs to this OCPP 2.0.1 module.
#[must_use]
pub fn owns(operation: &PrivilegedOcppOperation<Value>) -> bool {
    operation.protocol == ProtocolEdition::Ocpp201
        && action_index(operation.action.as_str()).is_some()
}

pub(crate) fn action_index(action: &str) -> Option<usize> {
    ACTIONS.iter().position(|candidate| *candidate == action)
}

/// Native request payload schema of one action.
pub(crate) const fn payload_schema(index: usize) -> &'static str {
    [
        "urn:OCPP:Cp:2:2020:3:SetVariableMonitoringRequest",
        "urn:OCPP:Cp:2:2020:3:ClearVariableMonitoringRequest",
        "urn:OCPP:Cp:2:2020:3:GetMonitoringReportRequest",
        "urn:OCPP:Cp:2:2020:3:SetMonitoringBaseRequest",
        "urn:OCPP:Cp:2:2020:3:SetMonitoringLevelRequest",
    ][index]
}

/// Validates the exact native payload on the station root and converts it to the typed query.
/// Everything the charger would refuse, or that this bridge cannot represent exactly, is refused
/// here, before any transmission.
///
/// # Errors
/// `UnsupportedOperation` for another action or edition and `InvalidParameters` for every
/// schema, scope, bound or semantic mismatch.
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<MonitoringQuery201, CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, UnsupportedOperation};

    if !owns(operation) {
        return Err(UnsupportedOperation);
    }
    let index = action_index(operation.action.as_str()).ok_or(UnsupportedOperation)?;
    let payload = &operation.payload;
    if operation.payload_schema.as_str() != payload_schema(index)
        || !station_scope(resource)
        || !valid_schema(request_schema(index), payload)
        || opaque(payload)
        || !clean(payload)
    {
        return Err(InvalidParameters);
    }

    let query = match index {
        0 => set(payload),
        1 => clear(payload),
        2 => report(payload),
        3 => MonitoringBase201::deserialize(&payload["monitoringBase"])
            .ok()
            .map(|base| MonitoringQuery201::SetMonitoringBase { base }),
        _ => severity(&payload["severity"])
            .map(|severity| MonitoringQuery201::SetMonitoringLevel { severity }),
    };

    query.ok_or(InvalidParameters)
}

fn set(payload: &Value) -> Option<MonitoringQuery201> {
    let items = payload["setMonitoringData"]
        .as_array()
        .filter(|items| items.len() <= MAXIMUM_ITEMS)?;
    let monitors = items
        .iter()
        .map(set_monitor)
        .collect::<Option<Vec<SetMonitor201>>>()?;

    let mut identities = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for monitor in &monitors {
        let negative_delta =
            monitor.monitor_type == MonitorType201::Delta && monitor.value.coefficient() < 0;
        let key = monitor_key(
            &monitor.component,
            &monitor.variable,
            monitor.monitor_type,
            monitor.severity,
        );
        if negative_delta || !identities.insert(key) || monitor.id.is_some_and(|id| !ids.insert(id))
        {
            return None;
        }
    }

    Some(MonitoringQuery201::SetVariableMonitoring { monitors })
}

fn clear(payload: &Value) -> Option<MonitoringQuery201> {
    let items = payload["id"]
        .as_array()
        .filter(|items| items.len() <= MAXIMUM_ITEMS)?;
    let ids = items.iter().map(int32).collect::<Option<Vec<i32>>>()?;

    let unique = ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != ids.len() {
        return None;
    }

    Some(MonitoringQuery201::ClearVariableMonitoring { ids })
}

fn report(payload: &Value) -> Option<MonitoringQuery201> {
    let criteria = match payload.get("monitoringCriteria") {
        None => Vec::new(),
        Some(values) => values
            .as_array()?
            .iter()
            .map(MonitoringCriterion201::deserialize)
            .collect::<Result<Vec<_>, _>>()
            .ok()?,
    };
    let component_variables = match payload.get("componentVariable") {
        None => Vec::new(),
        Some(values) => selectors(
            values
                .as_array()
                .filter(|items| items.len() <= MAXIMUM_ITEMS)?,
        )?,
    };

    Some(MonitoringQuery201::GetMonitoringReport {
        request_id: int32(&payload["requestId"])?,
        criteria,
        component_variables,
    })
}

fn selectors(items: &[Value]) -> Option<Vec<DeviceSelector201>> {
    use crate::v201::remote_control::device_model_values::{component, variable};

    items
        .iter()
        .map(|item| {
            Some(DeviceSelector201 {
                component: component(&item["component"])?,
                variable: match item.get("variable") {
                    None => None,
                    Some(variable_value) => Some(variable(variable_value)?),
                },
            })
        })
        .collect()
}
