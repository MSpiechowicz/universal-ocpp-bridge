//! The monitor-changing operations: `SetVariableMonitoring` (N04), `ClearVariableMonitoring`
//! (N06), `SetMonitoringBase` (N03) and `SetMonitoringLevel` (N05).
use super::config::MonitoringBase201;
use super::events::Draft;
use super::model::{Monitor, Origin, State, acceptable_value, same_component, same_variable};
use super::request::SetItem;
use serde_json::{Value, json};
use time::OffsetDateTime;

/// A refused item: its `SetMonitoringStatusEnumType` and an optional reason code.
struct Refusal {
    status: &'static str,
    reason: Option<&'static str>,
}

const fn rejected(reason: &'static str) -> Refusal {
    Refusal {
        status: "Rejected",
        reason: Some(reason),
    }
}

const fn refused(status: &'static str) -> Refusal {
    Refusal {
        status,
        reason: None,
    }
}

impl State {
    /// One result per item, in request order (N04.FR.01).
    pub(super) fn set_monitoring(&mut self, items: &[SetItem], now: OffsetDateTime) -> Vec<Value> {
        let mut drafts = Vec::new();
        let results = items
            .iter()
            .map(|item| {
                let mut result = json!({"type": item.monitor_type.as_str(), "severity": item.severity,
                    "component": item.raw_component, "variable": item.raw_variable});
                match self.set_one(item, now, &mut drafts) {
                    Ok(id) => {
                        result["id"] = json!(id);
                        result["status"] = json!("Accepted");
                    }
                    Err(refusal) => {
                        result["status"] = json!(refusal.status);
                        if let Some(reason) = refusal.reason {
                            result["statusInfo"] = json!({"reasonCode": reason});
                        }
                    }
                }
                result
            })
            .collect();
        // A full outbox drops the events, which the next change raises again.
        let _ = self.flush(drafts, now);
        results
    }

    fn set_one(
        &mut self,
        item: &SetItem,
        now: OffsetDateTime,
        drafts: &mut Vec<Draft>,
    ) -> Result<i32, Refusal> {
        if !self
            .variables
            .iter()
            .any(|entry| same_component(&entry.component, &item.component))
        {
            return Err(refused("UnknownComponent"));
        }
        let entry = self
            .variable(&item.component, &item.variable)
            .ok_or(refused("UnknownVariable"))?;
        if !entry.monitor_types.contains(&item.monitor_type) {
            return Err(refused("UnsupportedMonitorType"));
        }
        if !(0..=9).contains(&item.severity) {
            return Err(rejected("InvalidSeverity"));
        }
        acceptable_value(item.monitor_type, item.value, entry.min, entry.max).map_err(rejected)?;
        let current = entry.value.clone();
        let existing = match item.id {
            Some(id) => Some(self.replaceable(id, item)?),
            None => None,
        };
        let severity = u8::try_from(item.severity).unwrap_or(9);
        // N04.FR.10: one monitor of a type and severity per variable.
        let duplicate = self.monitors.iter().enumerate().any(|(index, other)| {
            Some(index) != existing
                && same_component(&other.component, &item.component)
                && same_variable(&other.variable, &item.variable)
                && other.kind == item.monitor_type
                && other.severity == severity
        });
        if duplicate {
            return Err(refused("Duplicate"));
        }
        let index = match existing {
            Some(index) => index,
            None => self.insert(item, severity)?,
        };
        let monitor = &mut self.monitors[index];
        monitor.origin = Origin::Custom;
        monitor.kind = item.monitor_type;
        monitor.value = item.value;
        monitor.severity = severity;
        monitor.transaction = item.transaction;
        let id = monitor.id;
        self.arm(index, now);
        // N07.FR.11/16/17: judge a new or changed threshold against the current value at once.
        if item.monitor_type.is_threshold() {
            drafts.extend(self.judge(index, &current));
        }
        Ok(id)
    }

    /// The monitor an id-bearing item replaces (N04.FR.12/13/16/18).
    fn replaceable(&self, id: i32, item: &SetItem) -> Result<usize, Refusal> {
        let index = self
            .monitors
            .iter()
            .position(|monitor| monitor.id == id)
            .ok_or(rejected("UnknownId"))?;
        let monitor = &self.monitors[index];
        if !same_component(&monitor.component, &item.component)
            || !same_variable(&monitor.variable, &item.variable)
        {
            return Err(rejected("ComponentMismatch"));
        }
        if monitor.origin == Origin::HardWired {
            return Err(rejected("HardWiredMonitor"));
        }
        Ok(index)
    }

    fn insert(&mut self, item: &SetItem, severity: u8) -> Result<usize, Refusal> {
        let capacity = usize::try_from(self.config.max_monitors).unwrap_or(0);
        if self.monitors.len() >= capacity {
            return Err(rejected("NoCapacity"));
        }
        let id = self.allocate_id();
        self.monitors.push(Monitor {
            id,
            origin: Origin::Custom,
            component: item.component.clone(),
            variable: item.variable.clone(),
            kind: item.monitor_type,
            value: item.value,
            severity,
            transaction: item.transaction,
            triggered: false,
            baseline: String::new(),
            next_due: None,
        });
        Ok(self.monitors.len() - 1)
    }

    /// One result per id (N06.FR.05). A removed exceeded threshold sends no cleared event
    /// (N07.FR.12).
    pub(super) fn clear_monitoring(&mut self, ids: &[i32]) -> Vec<Value> {
        ids.iter()
            .map(|id| {
                let Some(index) = self.monitors.iter().position(|monitor| monitor.id == *id) else {
                    return json!({"id": id, "status": "NotFound"});
                };
                if self.monitors[index].origin == Origin::HardWired {
                    return json!({"id": id, "status": "Rejected",
                        "statusInfo": {"reasonCode": "HardWired"}});
                }
                self.monitors.remove(index);
                json!({"id": id, "status": "Accepted"})
            })
            .collect()
    }

    /// N03.FR.03-05. `Err` carries the reason a base cannot be applied.
    pub(super) fn set_base(
        &mut self,
        base: MonitoringBase201,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        if !self.config.supported_bases.contains(&base) {
            return Err("NotSupported");
        }
        let mut monitors: Vec<Monitor> = self
            .monitors
            .iter()
            .filter(|monitor| base == MonitoringBase201::All || monitor.origin == Origin::HardWired)
            .cloned()
            .collect();
        if base != MonitoringBase201::HardWiredOnly {
            // Preconfigured monitors return as the factory set them, unless a custom monitor
            // took over their id (kept by All).
            for preset in &self.factory {
                if !monitors.iter().any(|monitor| monitor.id == preset.id) {
                    monitors.push(preset.clone());
                }
            }
        }
        if monitors.len() > usize::try_from(self.config.max_monitors).unwrap_or(0) {
            return Err("NoCapacity");
        }
        monitors.sort_by_key(|monitor| monitor.id);
        self.monitors = monitors;
        self.base = base;
        for index in 0..self.monitors.len() {
            self.arm(index, now);
        }
        Ok(())
    }
}
