//! Native replies of the privileged monitoring actions (N03 to N06). A reply is evidence only
//! when it is schema valid and corresponds to the exact request; anything else has no
//! authoritative meaning and is reported as an uncertain transmission, never as a success.
use crate::command_registry::monitoring201::{
    parse::{MonitorKey, clean, int32, monitor_key, optional_int32, severity},
    response_schema, valid_schema,
};
use crate::v201::remote_control::device_model_values::{component, variable};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use uob_contracts::{
    ClearMonitorResult201, ClearMonitoringStatus201, DeviceReportAck201, MonitorType201,
    MonitoringLevelStatus201, MonitoringOutcome201, MonitoringQuery201, MonitoringResult201,
    SetMonitor201, SetMonitorResult201, SetMonitoringStatus201, valid_monitoring_reason_code_201,
};

const SET: usize = 0;
const CLEAR: usize = 1;
const REPORT: usize = 2;
const BASE: usize = 3;
const LEVEL: usize = 4;

/// The native reply of every action except `GetMonitoringReport`, whose reply opens a collection.
pub(super) fn response(query: &MonitoringQuery201, payload: &Value) -> Option<MonitoringResult201> {
    let outcome = match query {
        MonitoringQuery201::SetVariableMonitoring { monitors } => {
            MonitoringOutcome201::SetMonitors {
                monitors: set(monitors, payload)?,
            }
        }
        MonitoringQuery201::ClearVariableMonitoring { ids } => {
            MonitoringOutcome201::ClearMonitors {
                monitors: clear(ids, payload)?,
            }
        }
        MonitoringQuery201::SetMonitoringBase { .. } => MonitoringOutcome201::Base {
            status: status(BASE, payload)?,
        },
        MonitoringQuery201::SetMonitoringLevel { .. } => MonitoringOutcome201::Level {
            status: MonitoringLevelStatus201::deserialize(&checked(LEVEL, payload)?["status"])
                .ok()?,
        },
        MonitoringQuery201::GetMonitoringReport { .. } => return None,
    };

    Some(MonitoringResult201 {
        query: query.clone(),
        outcome,
    })
}

/// The native acknowledgement of `GetMonitoringReport`.
pub(super) fn report_ack(payload: &Value) -> Option<DeviceReportAck201> {
    status(REPORT, payload)
}

fn checked(action: usize, payload: &Value) -> Option<&Value> {
    (valid_schema(response_schema(action), payload) && clean(payload)).then_some(payload)
}

fn status(action: usize, payload: &Value) -> Option<DeviceReportAck201> {
    DeviceReportAck201::deserialize(&checked(action, payload)?["status"]).ok()
}

/// The native `statusInfo.additionalInfo` free text is never retained.
fn reason_code(result: &Value) -> Option<String> {
    result["statusInfo"]["reasonCode"]
        .as_str()
        .filter(|code| valid_monitoring_reason_code_201(code))
        .map(str::to_owned)
}

/// Exactly one result per requested monitor (N04.FR.01, 02), matched on component, variable,
/// type and severity in any order. An accepted monitor carries its ID (N04.FR.11) and, when the
/// request replaced one, the same ID (N04.FR.12).
fn set(requested: &[SetMonitor201], payload: &Value) -> Option<Vec<SetMonitorResult201>> {
    let results = checked(SET, payload)?["setMonitoringResult"].as_array()?;
    if results.len() != requested.len() {
        return None;
    }
    let mut unanswered: BTreeMap<MonitorKey, &SetMonitor201> = requested
        .iter()
        .map(|item| {
            let key = monitor_key(
                &item.component,
                &item.variable,
                item.monitor_type,
                item.severity,
            );
            (key, item)
        })
        .collect();
    if unanswered.len() != requested.len() {
        return None;
    }

    let mut answers = Vec::with_capacity(results.len());
    for result in results {
        let component = component(&result["component"])?;
        let variable = variable(&result["variable"])?;
        let monitor_type = MonitorType201::deserialize(&result["type"]).ok()?;
        let severity = severity(&result["severity"])?;
        let item =
            unanswered.remove(&monitor_key(&component, &variable, monitor_type, severity))?;

        let status = SetMonitoringStatus201::deserialize(&result["status"]).ok()?;
        let id = optional_int32(result.get("id")).ok()?;
        if status.accepted() {
            let id = id?;
            if item.id.is_some_and(|requested| requested != id) {
                return None;
            }
        }
        answers.push(SetMonitorResult201 {
            // The native ID exists only for an accepted monitor.
            id: id.filter(|_| status.accepted()),
            status,
            reason_code: reason_code(result),
            monitor_type,
            severity,
            component,
            variable,
        });
    }

    Some(answers)
}

/// One result per requested ID, no more and no fewer (N06.FR.05).
fn clear(requested: &[i32], payload: &Value) -> Option<Vec<ClearMonitorResult201>> {
    let results = checked(CLEAR, payload)?["clearMonitoringResult"].as_array()?;
    let mut answers = Vec::with_capacity(results.len());
    for result in results {
        answers.push(ClearMonitorResult201 {
            id: int32(&result["id"])?,
            status: ClearMonitoringStatus201::deserialize(&result["status"]).ok()?,
            reason_code: reason_code(result),
        });
    }

    let mut wanted = requested.to_vec();
    let mut answered: Vec<i32> = answers.iter().map(|answer| answer.id).collect();
    wanted.sort_unstable();
    answered.sort_unstable();

    (wanted == answered).then_some(answers)
}
