//! The part of the device model that monitoring owns: the `MonitoringCtrlr` limits and the
//! monitorable variables, as `GetVariables`, `GetBaseReport` and `GetReport` see them.
use super::config::{ComponentId201, VariableId201};
use super::model::{
    State, component_from_json, same_component, same_variable, selector_matches, variable_from_json,
};
use serde_json::{Value, json};

/// One variable of the monitoring device model.
#[derive(Clone, Debug)]
pub(crate) struct DeviceVariable201 {
    component: ComponentId201,
    variable: VariableId201,
    data_type: &'static str,
    value: String,
    minimum: Option<f64>,
    maximum: Option<f64>,
    values_list: Option<&'static str>,
    supports_monitoring: bool,
    /// A `MonitoringCtrlr` variable, which `componentCriteria` reports as available and enabled.
    controller: bool,
    /// Part of a `SummaryInventory`.
    summary: bool,
}

impl DeviceVariable201 {
    pub(crate) const fn supports_monitoring(&self) -> bool {
        self.supports_monitoring
    }

    pub(crate) const fn controller(&self) -> bool {
        self.controller
    }

    pub(crate) const fn summary(&self) -> bool {
        self.summary
    }

    pub(crate) fn matches_query(&self, component: &Value, variable: &Value) -> bool {
        same_component(&self.component, &component_from_json(component))
            && same_variable(&self.variable, &variable_from_json(variable))
    }

    /// A `GetReport` selector; omitted instance, EVSE and variable select every one.
    pub(crate) fn matches_selector(&self, selector: &Value) -> bool {
        let variable = selector.get("variable").map(variable_from_json);
        selector_matches(
            (
                &component_from_json(&selector["component"]),
                variable.as_ref(),
            ),
            &self.component,
            &self.variable,
        )
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// `ReportDataType`.
    pub(crate) fn report_json(&self) -> Value {
        let mut characteristics =
            json!({"dataType": self.data_type, "supportsMonitoring": self.supports_monitoring});
        if let Some(minimum) = self.minimum {
            characteristics["minLimit"] = json!(minimum);
        }
        if let Some(maximum) = self.maximum {
            characteristics["maxLimit"] = json!(maximum);
        }
        if let Some(list) = self.values_list {
            characteristics["valuesList"] = json!(list);
        }
        json!({"component": self.component.to_json(), "variable": self.variable.to_json(),
            "variableAttribute": [{"type": "Actual", "value": self.value, "mutability": "ReadOnly"}],
            "variableCharacteristics": characteristics})
    }
}

pub(crate) fn knows_component(variables: &[DeviceVariable201], component: &Value) -> bool {
    let wanted = component_from_json(component);
    variables
        .iter()
        .any(|variable| same_component(&variable.component, &wanted))
}

fn controller(
    name: &str,
    instance: Option<&str>,
    data_type: &'static str,
    value: String,
    summary: bool,
) -> DeviceVariable201 {
    DeviceVariable201 {
        component: ComponentId201 {
            name: "MonitoringCtrlr".to_owned(),
            instance: None,
            evse: None,
        },
        variable: VariableId201 {
            name: name.to_owned(),
            instance: instance.map(str::to_owned),
        },
        data_type,
        value,
        minimum: None,
        maximum: None,
        values_list: None,
        supports_monitoring: false,
        controller: true,
        summary,
    }
}

impl State {
    pub(super) fn device_variables(&self) -> Vec<DeviceVariable201> {
        let config = &self.config;
        let mut variables = vec![
            controller("Available", None, "boolean", "true".to_owned(), true),
            controller("Enabled", None, "boolean", "true".to_owned(), true),
            controller(
                "ItemsPerMessage",
                Some("SetVariableMonitoring"),
                "integer",
                config.items_per_message_set.to_string(),
                false,
            ),
            controller(
                "ItemsPerMessage",
                Some("ClearVariableMonitoring"),
                "integer",
                config.items_per_message_clear.to_string(),
                false,
            ),
            controller(
                "BytesPerMessage",
                Some("SetVariableMonitoring"),
                "integer",
                config.bytes_per_message_set.to_string(),
                false,
            ),
            controller(
                "BytesPerMessage",
                Some("ClearVariableMonitoring"),
                "integer",
                config.bytes_per_message_clear.to_string(),
                false,
            ),
        ];
        let mut base = controller(
            "ActiveMonitoringBase",
            None,
            "OptionList",
            self.base.as_str().to_owned(),
            false,
        );
        base.values_list = Some("All,FactoryDefault,HardWiredOnly");
        let mut level = controller(
            "ActiveMonitoringLevel",
            None,
            "integer",
            self.level.to_string(),
            false,
        );
        level.minimum = Some(0.0);
        level.maximum = Some(9.0);
        let mut active = controller(
            "ActiveMonitors",
            None,
            "integer",
            self.monitors.len().to_string(),
            false,
        );
        active.minimum = Some(f64::from(
            u32::try_from(self.hard_wired()).unwrap_or(u32::MAX),
        ));
        active.maximum = Some(f64::from(config.max_monitors));
        variables.extend([base, level, active]);
        variables.extend(self.variables.iter().map(|entry| DeviceVariable201 {
            component: entry.component.clone(),
            variable: entry.variable.clone(),
            data_type: entry.data_type.as_str(),
            value: entry.value.clone(),
            minimum: entry.min,
            maximum: entry.max,
            values_list: None,
            supports_monitoring: !entry.monitor_types.is_empty(),
            controller: false,
            summary: false,
        }));
        variables
    }
}
