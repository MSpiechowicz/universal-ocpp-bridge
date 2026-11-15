//! Monitoring state: the seeded variables, the monitors in force and the match rules that tie a
//! request's component and variable to them.
use super::config::{
    ComponentId201, EvseId201, MonitorType201, MonitoredVariable201, Monitoring201Config,
    MonitoringBase201, SeededKind201, VariableId201,
};
use super::events::EventMessage201;
use caseless::Caseless;
use serde_json::{Value, json};
use std::collections::VecDeque;
use time::OffsetDateTime;

/// Who owns a monitor now. A replaced preconfigured monitor becomes custom (N04.FR.15).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Origin {
    HardWired,
    Preconfigured,
    Custom,
}

impl Origin {
    /// `EventNotificationEnumType` of an event this monitor raised.
    pub(super) const fn notification_type(self) -> &'static str {
        match self {
            Self::HardWired => "HardWiredMonitor",
            Self::Preconfigured => "PreconfiguredMonitor",
            Self::Custom => "CustomMonitor",
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::HardWired => "HardWired",
            Self::Preconfigured => "Preconfigured",
            Self::Custom => "Custom",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Monitor {
    pub(super) id: i32,
    pub(super) origin: Origin,
    pub(super) component: ComponentId201,
    pub(super) variable: VariableId201,
    pub(super) kind: MonitorType201,
    pub(super) value: f64,
    pub(super) severity: u8,
    pub(super) transaction: bool,
    /// A threshold monitor is currently exceeded, so the next return to normal is reported.
    pub(super) triggered: bool,
    /// The value a delta monitor measures change from.
    pub(super) baseline: String,
    pub(super) next_due: Option<OffsetDateTime>,
}

impl Monitor {
    /// `VariableMonitoringType` as reported by `NotifyMonitoringReport`.
    pub(super) fn report_json(&self) -> Value {
        json!({"id": self.id, "transaction": self.transaction, "value": self.value,
            "type": self.kind.as_str(), "severity": self.severity})
    }
}

/// Events and reports counted for scenario assertions.
#[derive(Clone, Debug, Default)]
pub(super) struct Stats {
    pub(super) events_emitted: u64,
    pub(super) events_acknowledged: u64,
    pub(super) events_rejected: u64,
    pub(super) events_suppressed: u64,
    pub(super) last_event_outcome: Option<&'static str>,
    pub(super) reports_delivered: u64,
    pub(super) fragments_sent: u64,
    pub(super) last_report: Option<Value>,
}

pub(super) struct State {
    pub(super) config: Monitoring201Config,
    pub(super) variables: Vec<MonitoredVariable201>,
    pub(super) monitors: Vec<Monitor>,
    /// The preconfigured monitors as the factory set them (N03.FR.04).
    pub(super) factory: Vec<Monitor>,
    pub(super) next_id: i32,
    pub(super) base: MonitoringBase201,
    pub(super) level: u8,
    pub(super) outbox: VecDeque<EventMessage201>,
    pub(super) next_event_id: i32,
    pub(super) stats: Stats,
}

impl State {
    pub(super) fn new(config: &Monitoring201Config) -> Self {
        let seeded: Vec<Monitor> = config
            .monitors
            .iter()
            .map(|seed| Monitor {
                id: seed.id,
                origin: match seed.kind {
                    SeededKind201::HardWired => Origin::HardWired,
                    SeededKind201::Preconfigured => Origin::Preconfigured,
                },
                component: seed.component.clone(),
                variable: seed.variable.clone(),
                kind: seed.monitor_type,
                value: seed.value,
                severity: seed.severity,
                transaction: seed.transaction,
                triggered: false,
                baseline: String::new(),
                next_due: None,
            })
            .collect();
        let factory = seeded
            .iter()
            .filter(|monitor| monitor.origin == Origin::Preconfigured)
            .cloned()
            .collect();
        let next_id = seeded.iter().map(|monitor| monitor.id).max().unwrap_or(0) + 1;
        let mut state = Self {
            config: config.clone(),
            variables: config.variables.clone(),
            monitors: seeded,
            factory,
            next_id,
            base: MonitoringBase201::FactoryDefault,
            level: config.initial_level,
            outbox: VecDeque::new(),
            next_event_id: 1,
            stats: Stats::default(),
        };
        let now = OffsetDateTime::now_utc();
        for index in 0..state.monitors.len() {
            state.arm(index, now);
        }
        state
    }

    /// Reset a monitor's runtime state against the variable's current value.
    pub(super) fn arm(&mut self, index: usize, now: OffsetDateTime) {
        let current = self
            .variable(
                &self.monitors[index].component,
                &self.monitors[index].variable,
            )
            .map(|variable| variable.value.clone())
            .unwrap_or_default();
        let monitor = &mut self.monitors[index];
        monitor.baseline = current;
        monitor.next_due = match monitor.kind {
            MonitorType201::Periodic => interval(monitor.value).map(|span| now + span),
            MonitorType201::PeriodicClockAligned => aligned_after(monitor.value, now),
            _ => None,
        };
    }

    pub(super) fn variable(
        &self,
        component: &ComponentId201,
        variable: &VariableId201,
    ) -> Option<&MonitoredVariable201> {
        self.variables.iter().find(|entry| {
            same_component(&entry.component, component) && same_variable(&entry.variable, variable)
        })
    }

    pub(super) fn allocate_id(&mut self) -> i32 {
        loop {
            let candidate = self.next_id;
            self.next_id += 1;
            let taken = self
                .monitors
                .iter()
                .chain(&self.factory)
                .any(|monitor| monitor.id == candidate);
            if !taken {
                return candidate;
            }
        }
    }

    pub(super) fn hard_wired(&self) -> usize {
        self.monitors
            .iter()
            .filter(|monitor| monitor.origin == Origin::HardWired)
            .count()
    }
}

fn fold_eq(left: &str, right: &str) -> bool {
    left.chars()
        .default_case_fold()
        .eq(right.chars().default_case_fold())
}

fn optional_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => fold_eq(left, right),
        _ => false,
    }
}

fn evse_eq(left: Option<&EvseId201>, right: Option<&EvseId201>) -> bool {
    left == right
}

/// Exact identity of a component (names are case insensitive).
pub(crate) fn same_component(left: &ComponentId201, right: &ComponentId201) -> bool {
    fold_eq(&left.name, &right.name)
        && optional_eq(left.instance.as_deref(), right.instance.as_deref())
        && evse_eq(left.evse.as_ref(), right.evse.as_ref())
}

/// Exact identity of a variable (names are case insensitive).
pub(crate) fn same_variable(left: &VariableId201, right: &VariableId201) -> bool {
    fold_eq(&left.name, &right.name)
        && optional_eq(left.instance.as_deref(), right.instance.as_deref())
}

/// A report selector: omitted instance, EVSE, connector or variable select every one
/// (N02.FR.16-21).
pub(crate) fn selector_matches(
    selector: (&ComponentId201, Option<&VariableId201>),
    component: &ComponentId201,
    variable: &VariableId201,
) -> bool {
    let (wanted, wanted_variable) = selector;
    let evse = wanted.evse.as_ref().is_none_or(|evse| {
        component.evse.as_ref().is_some_and(|actual| {
            actual.id == evse.id
                && evse
                    .connector_id
                    .is_none_or(|id| actual.connector_id == Some(id))
        })
    });
    fold_eq(&wanted.name, &component.name)
        && wanted.instance.as_deref().is_none_or(|instance| {
            component
                .instance
                .as_deref()
                .is_some_and(|actual| fold_eq(instance, actual))
        })
        && evse
        && wanted_variable.is_none_or(|wanted| {
            fold_eq(&wanted.name, &variable.name)
                && wanted.instance.as_deref().is_none_or(|instance| {
                    variable
                        .instance
                        .as_deref()
                        .is_some_and(|actual| fold_eq(instance, actual))
                })
        })
}

/// Why a monitor value is refused (N04.FR.06, N04.FR.14 and N04.FR.20).
pub(crate) fn acceptable_value(
    monitor_type: MonitorType201,
    value: f64,
    minimum: Option<f64>,
    maximum: Option<f64>,
) -> Result<(), &'static str> {
    if !value.is_finite() {
        return Err("InvalidValue");
    }
    match monitor_type {
        MonitorType201::UpperThreshold | MonitorType201::LowerThreshold
            if minimum.is_some_and(|limit| value < limit)
                || maximum.is_some_and(|limit| value > limit) =>
        {
            Err("ValueOutOfRange")
        }
        MonitorType201::Delta if value <= 0.0 => Err("InvalidDelta"),
        MonitorType201::Periodic | MonitorType201::PeriodicClockAligned if value <= 0.0 => {
            Err("InvalidInterval")
        }
        _ => Ok(()),
    }
}

/// A periodic interval as a duration; sub-millisecond or absurd intervals are not representable.
pub(super) fn interval(seconds: f64) -> Option<time::Duration> {
    (seconds.is_finite() && (0.001..=31_536_000.0).contains(&seconds))
        .then(|| time::Duration::seconds_f64(seconds))
}

/// The next clock-aligned instant after `now` for a whole-second interval (N08.FR.07).
pub(super) fn aligned_after(seconds: f64, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let span = interval(seconds)?;
    let step = span.whole_seconds().max(1);
    let next = (now.unix_timestamp().div_euclid(step) + 1) * step;
    OffsetDateTime::from_unix_timestamp(next).ok()
}

/// Component identity from native JSON that the request schema already validated.
pub(crate) fn component_from_json(value: &Value) -> ComponentId201 {
    let evse = value.get("evse").map(|evse| EvseId201 {
        id: evse["id"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .unwrap_or(0),
        connector_id: evse
            .get("connectorId")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok()),
    });
    ComponentId201 {
        name: value["name"].as_str().unwrap_or_default().to_owned(),
        instance: value
            .get("instance")
            .and_then(Value::as_str)
            .map(str::to_owned),
        evse,
    }
}

pub(crate) fn variable_from_json(value: &Value) -> VariableId201 {
    VariableId201 {
        name: value["name"].as_str().unwrap_or_default().to_owned(),
        instance: value
            .get("instance")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}
