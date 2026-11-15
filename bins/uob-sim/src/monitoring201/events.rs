//! Events raised by monitors (use cases N07 and N08) and the outbox that delivers them as
//! `NotifyEvent` messages.
use super::config::{ComponentId201, MonitorType201, VariableId201};
use super::model::{
    Monitor, Origin, State, aligned_after, interval, same_component, same_variable,
};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Messages waiting for delivery; a larger backlog drops new events rather than a chain's middle.
const OUTBOX_LIMIT: usize = 4096;

/// One `NotifyEvent` CALL. The messages of one event chain share consecutive `seqNo`s from 0 and
/// carry `tbc` until the last one (N07.FR.07).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMessage201 {
    pub seq_no: u32,
    pub tbc: bool,
    pub generated_at: String,
    pub event_data: Vec<Value>,
}

impl EventMessage201 {
    /// The `NotifyEventRequest` payload.
    #[must_use]
    pub fn to_request(&self) -> Value {
        json!({"generatedAt": self.generated_at, "seqNo": self.seq_no, "tbc": self.tbc,
            "eventData": self.event_data})
    }
}

/// An event a monitor raised, before it gets an `eventId`.
pub(super) struct Draft {
    monitor: i32,
    origin: Origin,
    component: ComponentId201,
    variable: VariableId201,
    trigger: &'static str,
    cleared: bool,
    severity: u8,
    actual: String,
}

impl Draft {
    pub(super) fn new(monitor: &Monitor, cleared: bool, actual: &str) -> Self {
        let trigger = match monitor.kind {
            MonitorType201::UpperThreshold | MonitorType201::LowerThreshold => "Alerting",
            MonitorType201::Delta => "Delta",
            MonitorType201::Periodic | MonitorType201::PeriodicClockAligned => "Periodic",
        };
        Self {
            monitor: monitor.id,
            origin: monitor.origin,
            component: monitor.component.clone(),
            variable: monitor.variable.clone(),
            trigger,
            cleared,
            severity: monitor.severity,
            actual: actual.to_owned(),
        }
    }

    /// `EventDataType` (N07.FR.06: component, variable and the monitor that caused the event).
    fn to_json(&self, event_id: i32, timestamp: &str) -> Value {
        let mut event = json!({
            "eventId": event_id,
            "timestamp": timestamp,
            "trigger": self.trigger,
            "actualValue": self.actual,
            "eventNotificationType": self.origin.notification_type(),
            "component": self.component.to_json(),
            "variable": self.variable.to_json(),
            "variableMonitoringId": self.monitor,
        });
        if self.cleared {
            event["cleared"] = json!(true);
        }
        event
    }
}

pub(super) fn timestamp(now: OffsetDateTime) -> String {
    now.replace_nanosecond(0)
        .expect("zero nanoseconds are valid")
        .format(&Rfc3339)
        .expect("UTC timestamp formats")
}

impl State {
    /// Judge one monitor against the variable's value, updating what it remembers.
    pub(super) fn judge(&mut self, index: usize, current: &str) -> Option<Draft> {
        let numeric_variable = {
            let monitor = &self.monitors[index];
            self.variable(&monitor.component, &monitor.variable)
                .is_some_and(|variable| variable.data_type.numeric())
        };
        let monitor = &mut self.monitors[index];
        let number = current
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite());
        match monitor.kind {
            MonitorType201::UpperThreshold | MonitorType201::LowerThreshold => {
                let number = number?;
                // N07.FR.16/17: events fire past the threshold, not on it.
                let exceeded = if monitor.kind == MonitorType201::UpperThreshold {
                    number > monitor.value
                } else {
                    number < monitor.value
                };
                if exceeded == monitor.triggered {
                    return None;
                }
                monitor.triggered = exceeded;
                // N07.FR.02: a return to normal is reported with cleared=true.
                Some(Draft::new(monitor, !exceeded, current))
            }
            MonitorType201::Delta => {
                let baseline = monitor.baseline.parse::<f64>().ok();
                let changed = match (numeric_variable, number, baseline) {
                    // N07.FR.18: more than plus or minus the value since the last notice.
                    (true, Some(number), Some(baseline)) => {
                        (number - baseline).abs() > monitor.value
                    }
                    // N07.FR.19: any other variable reports every change.
                    _ => current != monitor.baseline,
                };
                if !changed {
                    return None;
                }
                current.clone_into(&mut monitor.baseline);
                Some(Draft::new(monitor, false, current))
            }
            MonitorType201::Periodic | MonitorType201::PeriodicClockAligned => None,
        }
    }

    /// Queue drafts as one `NotifyEvent` chain, dropping events above the monitoring level
    /// (N07.FR.15). Returns the number of events queued.
    pub(super) fn flush(
        &mut self,
        drafts: Vec<Draft>,
        now: OffsetDateTime,
    ) -> Result<usize, &'static str> {
        let (kept, hidden): (Vec<Draft>, Vec<Draft>) = drafts
            .into_iter()
            .partition(|draft| draft.severity <= self.level);
        self.stats.events_suppressed += hidden.len() as u64;
        if kept.is_empty() {
            return Ok(0);
        }
        let per_message = usize::try_from(self.config.events_per_message).unwrap_or(1);
        let messages = kept.len().div_ceil(per_message);
        if self.outbox.len() + messages > OUTBOX_LIMIT {
            return Err("monitoring_outbox_full");
        }
        let generated = timestamp(now);
        let events: Vec<Value> = kept
            .iter()
            .map(|draft| {
                let id = self.next_event_id;
                self.next_event_id = id.checked_add(1).unwrap_or(1);
                draft.to_json(id, &generated)
            })
            .collect();
        for (number, chunk) in events.chunks(per_message).enumerate() {
            self.outbox.push_back(EventMessage201 {
                seq_no: u32::try_from(number).unwrap_or(u32::MAX),
                tbc: number + 1 < messages,
                generated_at: generated.clone(),
                event_data: chunk.to_vec(),
            });
        }
        self.stats.events_emitted += events.len() as u64;
        Ok(events.len())
    }

    /// Change a variable's value and raise whatever its monitors judge (scenario step).
    pub(super) fn change_value(
        &mut self,
        component: &ComponentId201,
        variable: &VariableId201,
        value: &str,
        now: OffsetDateTime,
    ) -> Result<usize, &'static str> {
        let entry = self
            .variables
            .iter_mut()
            .find(|entry| {
                same_component(&entry.component, component)
                    && same_variable(&entry.variable, variable)
            })
            .ok_or("monitored_variable_unknown")?;
        let numeric = entry.data_type.numeric();
        let boolean = entry.data_type == super::config::VariableType201::Boolean;
        let valid = value.chars().count() <= 2500
            && (!numeric || value.parse::<f64>().is_ok_and(f64::is_finite))
            && (!boolean || matches!(value, "true" | "false"));
        if !valid {
            return Err("monitored_value_invalid");
        }
        value.clone_into(&mut entry.value);
        let drafts = self.judge_variable(component, variable, value);
        self.flush(drafts, now)
    }

    /// Judge every monitor on one variable against `value`.
    pub(super) fn judge_variable(
        &mut self,
        component: &ComponentId201,
        variable: &VariableId201,
        value: &str,
    ) -> Vec<Draft> {
        let indices: Vec<usize> = self
            .monitors
            .iter()
            .enumerate()
            .filter(|(_, monitor)| {
                same_component(&monitor.component, component)
                    && same_variable(&monitor.variable, variable)
            })
            .map(|(index, _)| index)
            .collect();
        indices
            .into_iter()
            .filter_map(|index| self.judge(index, value))
            .collect()
    }

    /// Raise `count` events from one monitor regardless of the value (scenario burst).
    pub(super) fn burst(
        &mut self,
        component: &ComponentId201,
        variable: &VariableId201,
        count: usize,
        monitor: Option<i32>,
        now: OffsetDateTime,
    ) -> Result<usize, &'static str> {
        let current = self
            .variable(component, variable)
            .ok_or("monitored_variable_unknown")?
            .value
            .clone();
        let chosen = self
            .monitors
            .iter()
            .filter(|candidate| {
                same_component(&candidate.component, component)
                    && same_variable(&candidate.variable, variable)
            })
            .find(|candidate| monitor.is_none_or(|id| candidate.id == id))
            .ok_or("monitor_not_found")?;
        let drafts = (0..count)
            .map(|_| Draft::new(chosen, false, &current))
            .collect();
        self.flush(drafts, now)
    }

    /// Raise every periodic event that came due (N08.FR.06/07) and schedule the next one.
    pub(super) fn periodic(&mut self, now: OffsetDateTime) {
        let mut drafts = Vec::new();
        for index in 0..self.monitors.len() {
            let due = self.monitors[index].next_due.is_some_and(|due| due <= now);
            if !due {
                continue;
            }
            let monitor = &self.monitors[index];
            let current = self
                .variable(&monitor.component, &monitor.variable)
                .map(|variable| variable.value.clone())
                .unwrap_or_default();
            drafts.push(Draft::new(monitor, false, &current));
            let monitor = &mut self.monitors[index];
            monitor.next_due = if monitor.kind == MonitorType201::Periodic {
                interval(monitor.value).map(|span| now + span)
            } else {
                aligned_after(monitor.value, now)
            };
        }
        // A full outbox drops the periodic events; the next period raises them again.
        let _ = self.flush(drafts, now);
    }
}
