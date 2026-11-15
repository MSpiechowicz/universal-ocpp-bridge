//! `NotifyMonitoringReportRequest` fragments (N02) sanitized against their exact request, and
//! the filter semantics of `GetMonitoringReport`. Reported monitors are charger facts; they are
//! never adopted as local policy.
use crate::command_registry::monitoring201::parse::{clean, exact_number, int32, severity};
use crate::multipart::ReportFailure;
use crate::v201::remote_control::device_model_values::{component, variable};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use uob_contracts::{
    DeviceComponent201, DeviceSelector201, DeviceVariable201, MonitorType201,
    MonitoringCriterion201, MonitoringQuery201, MonitoringReportFragment201, ReportedMonitor201,
    UtcTimestamp,
};

#[cfg(test)]
#[path = "monitoring_values_tests.rs"]
mod tests;

pub(crate) struct SanitizedFragment {
    pub items: Vec<Vec<u8>>,
    pub metadata: MonitoringReportFragment201,
}

/// The caller validated the pinned schema and the request ID. A monitor outside the request's
/// criteria or component-variable filter is a correlation failure (N02.FR.05, 06, 08, 12 to 14,
/// 16 to 21); content the contract cannot carry exactly is an invalid fragment.
pub(crate) fn fragment(
    payload: &Value,
    query: &MonitoringQuery201,
) -> Result<SanitizedFragment, ReportFailure> {
    let MonitoringQuery201::GetMonitoringReport {
        criteria,
        component_variables,
        ..
    } = query
    else {
        return Err(ReportFailure::CorrelationMismatch);
    };
    let invalid = || ReportFailure::InvalidFragment;
    if !clean(payload) {
        return Err(invalid());
    }
    let sequence = payload["seqNo"]
        .as_u64()
        .and_then(|sequence| u32::try_from(sequence).ok())
        .ok_or_else(invalid)?;
    let generated_at = UtcTimestamp::deserialize(&payload["generatedAt"]).map_err(|_| invalid())?;
    let filter = Filter::new(criteria, component_variables);

    let mut items = Vec::new();
    for data in payload
        .get("monitor")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
    {
        let component = component(&data["component"]).ok_or_else(invalid)?;
        let variable = variable(&data["variable"]).ok_or_else(invalid)?;
        for raw in data["variableMonitoring"].as_array().ok_or_else(invalid)? {
            let monitor = reported(raw, &component, &variable).ok_or_else(invalid)?;
            if !filter.allows(&monitor) {
                return Err(ReportFailure::CorrelationMismatch);
            }
            items.push(serde_json::to_vec(&monitor).map_err(|_| ReportFailure::ByteLimit)?);
        }
    }

    let monitors = u32::try_from(items.len()).map_err(|_| ReportFailure::ItemLimit)?;
    Ok(SanitizedFragment {
        metadata: MonitoringReportFragment201 {
            generated_at,
            sequence,
            more: payload.get("tbc").and_then(Value::as_bool).unwrap_or(false),
            monitors,
        },
        items,
    })
}

fn reported(
    raw: &Value,
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
) -> Option<ReportedMonitor201> {
    Some(ReportedMonitor201 {
        component: component.clone(),
        variable: variable.clone(),
        id: int32(&raw["id"])?,
        transaction: raw["transaction"].as_bool()?,
        value: exact_number(&raw["value"])?,
        monitor_type: MonitorType201::deserialize(&raw["type"]).ok()?,
        severity: severity(&raw["severity"])?,
    })
}

const fn criterion(monitor_type: MonitorType201) -> MonitoringCriterion201 {
    match monitor_type {
        MonitorType201::UpperThreshold | MonitorType201::LowerThreshold => {
            MonitoringCriterion201::ThresholdMonitoring
        }
        MonitorType201::Delta => MonitoringCriterion201::DeltaMonitoring,
        MonitorType201::Periodic | MonitorType201::PeriodicClockAligned => {
            MonitoringCriterion201::PeriodicMonitoring
        }
    }
}

fn fold(text: &str) -> String {
    caseless::default_case_fold_str(text)
}

/// One requested selector with its names folded once. Absent parts are wildcards.
struct Selector {
    instance: Option<String>,
    evse: Option<(i32, Option<i32>)>,
    variable: Option<(String, Option<String>)>,
}

/// Criteria combine with OR among themselves; the criteria and the selectors both have to admit
/// a monitor. Selectors are indexed by folded component name so a large filter stays cheap.
pub(crate) struct Filter<'a> {
    criteria: &'a [MonitoringCriterion201],
    selectors: Option<BTreeMap<String, Vec<Selector>>>,
}

impl<'a> Filter<'a> {
    pub(crate) fn new(
        criteria: &'a [MonitoringCriterion201],
        selectors: &[DeviceSelector201],
    ) -> Self {
        let selectors = (!selectors.is_empty()).then(|| {
            let mut index: BTreeMap<String, Vec<Selector>> = BTreeMap::new();
            for selector in selectors {
                index
                    .entry(fold(&selector.component.name))
                    .or_default()
                    .push(Selector {
                        instance: selector.component.instance.as_deref().map(fold),
                        evse: selector
                            .component
                            .evse
                            .as_ref()
                            .map(|evse| (evse.id, evse.connector_id)),
                        variable: selector.variable.as_ref().map(|variable| {
                            (fold(&variable.name), variable.instance.as_deref().map(fold))
                        }),
                    });
            }
            index
        });

        Self {
            criteria,
            selectors,
        }
    }

    pub(crate) fn allows(&self, monitor: &ReportedMonitor201) -> bool {
        if !self.criteria.is_empty() && !self.criteria.contains(&criterion(monitor.monitor_type)) {
            return false;
        }
        let Some(selectors) = &self.selectors else {
            return true;
        };

        selectors
            .get(&fold(&monitor.component.name))
            .is_some_and(|candidates| {
                candidates
                    .iter()
                    .any(|selector| selects(selector, &monitor.component, &monitor.variable))
            })
    }
}

fn selects(
    selector: &Selector,
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
) -> bool {
    let instance = component.instance.as_deref().map(fold);
    if selector.instance.is_some() && selector.instance != instance {
        return false;
    }
    if let Some((evse, connector)) = selector.evse {
        let reported = component.evse.as_ref();
        if reported.map(|reported| reported.id) != Some(evse)
            || connector
                .is_some_and(|id| reported.and_then(|reported| reported.connector_id) != Some(id))
        {
            return false;
        }
    }
    let Some((name, variable_instance)) = &selector.variable else {
        return true;
    };

    fold(&variable.name) == *name
        && variable_instance
            .as_ref()
            .is_none_or(|wanted| variable.instance.as_deref().map(fold).as_ref() == Some(wanted))
}
