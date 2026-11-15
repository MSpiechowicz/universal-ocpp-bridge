//! Independent OCPP 2.0.1 station monitoring model: use cases N02 to N08 (`SetVariableMonitoring`,
//! `ClearVariableMonitoring`, `SetMonitoringBase`, `SetMonitoringLevel`, `GetMonitoringReport`,
//! `NotifyMonitoringReport` and `NotifyEvent`), written from the pinned Edition 4 text and JSON
//! schemas. The model is seeded from configuration and lives as long as the station.
mod config;
mod device;
mod events;
mod model;
mod ops;
mod report;
pub(crate) mod request;
pub(crate) mod transport;

pub use config::{
    ComponentId201, EvseId201, MonitorType201, MonitoredVariable201, Monitoring201Config,
    MonitoringBase201, MonitoringCriterion201, ReportFault201, SeededKind201, SeededMonitor201,
    SetReplyFault201, VariableId201, VariableType201,
};
pub(crate) use device::{DeviceVariable201, knows_component};
pub use events::EventMessage201;
use model::{State, component_from_json, variable_from_json};
use parking_lot::Mutex;
pub use report::ReportPlan201;
use serde_json::{Value, json};
use std::{fmt, sync::Arc};
use time::OffsetDateTime;

/// How the CSMS answered one `NotifyEvent`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventOutcome201 {
    /// An (empty) `NotifyEventResponse` (N07.FR.03).
    Acknowledged,
    /// A CALLERROR or an undecodable CALLRESULT: received, never resent.
    CallError,
    /// The call never reached the CSMS; it is resent.
    Failed,
}

/// The reply to one monitoring CALL: the CALLRESULT payload, or `{"callError": <code>}`, and for
/// an accepted `GetMonitoringReport` the fragments that follow.
#[derive(Clone, Debug)]
pub struct Reply201 {
    pub payload: Value,
    pub report: Option<ReportPlan201>,
}

struct Model {
    state: State,
    sending: bool,
    send_retry_at: Option<OffsetDateTime>,
}

#[derive(Clone)]
pub struct Monitoring201Handle(Arc<Mutex<Model>>);

impl fmt::Debug for Monitoring201Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Monitoring201Handle(<private>)")
    }
}

/// The monitoring CALLs a station with this model answers.
pub const MONITORING_ACTIONS_201: [&str; 5] = [
    "SetVariableMonitoring",
    "ClearVariableMonitoring",
    "GetMonitoringReport",
    "SetMonitoringBase",
    "SetMonitoringLevel",
];

impl Monitoring201Handle {
    /// Build the station's monitoring state from its configuration.
    /// # Errors
    /// Invalid configuration.
    pub fn open(config: &Monitoring201Config) -> Result<Self, &'static str> {
        config.validate()?;
        Ok(Self(Arc::new(Mutex::new(Model {
            state: State::new(config),
            sending: false,
            send_retry_at: None,
        }))))
    }

    /// Reply to a native monitoring CALL.
    #[must_use]
    pub fn handle_call(&self, action: &str, payload: &Value) -> Reply201 {
        let mut model = self.0.lock();
        match model.state.call(action, payload, OffsetDateTime::now_utc()) {
            Ok((payload, report)) => Reply201 { payload, report },
            Err(code) => Reply201 {
                payload: json!({"callError": code}),
                report: None,
            },
        }
    }

    /// Change a monitored variable and queue the events its monitors raise. Returns how many.
    /// # Errors
    /// An unknown variable or a value its data type does not allow.
    pub fn set_value(
        &self,
        component: &Value,
        variable: &Value,
        value: &str,
    ) -> Result<usize, &'static str> {
        self.0.lock().state.change_value(
            &component_from_json(component),
            &variable_from_json(variable),
            value,
            OffsetDateTime::now_utc(),
        )
    }

    /// Queue `count` events from one monitor of a variable, chained over several messages.
    /// # Errors
    /// An unknown variable, no such monitor, or a full outbox.
    pub fn emit_events(
        &self,
        component: &Value,
        variable: &Value,
        count: usize,
        monitor: Option<i32>,
    ) -> Result<usize, &'static str> {
        self.0.lock().state.burst(
            &component_from_json(component),
            &variable_from_json(variable),
            count,
            monitor,
            OffsetDateTime::now_utc(),
        )
    }

    /// Raise the periodic events that came due.
    pub fn advance(&self, now: OffsetDateTime) {
        self.0.lock().state.periodic(now);
    }

    /// Claim the oldest queued `NotifyEvent` for one in-flight CALL.
    #[must_use]
    pub fn next_event(&self, now: OffsetDateTime) -> Option<EventMessage201> {
        let mut model = self.0.lock();
        if model.sending || model.send_retry_at.is_some_and(|retry| retry > now) {
            return None;
        }
        let message = model.state.outbox.front().cloned()?;
        model.sending = true;
        Some(message)
    }

    /// Release a claimed message; a received one leaves the queue.
    pub fn event_finished(
        &self,
        message: &EventMessage201,
        outcome: EventOutcome201,
        now: OffsetDateTime,
    ) {
        let mut model = self.0.lock();
        model.sending = false;
        if outcome == EventOutcome201::Failed {
            model.send_retry_at = Some(now + time::Duration::seconds(1));
            return;
        }
        model.send_retry_at = None;
        if model.state.outbox.front() == Some(message) {
            model.state.outbox.pop_front();
        }
        let stats = &mut model.state.stats;
        if outcome == EventOutcome201::Acknowledged {
            stats.events_acknowledged += 1;
            stats.last_event_outcome = Some("Result");
        } else {
            stats.events_rejected += 1;
            stats.last_event_outcome = Some("CallError");
        }
    }

    /// Record how far a report got: `Complete`, `Withheld`, `Interrupted` or `Failed`.
    pub fn report_finished(&self, plan: &ReportPlan201, sent: usize, outcome: &str) {
        let mut model = self.0.lock();
        let stats = &mut model.state.stats;
        stats.fragments_sent += sent as u64;
        if outcome == "Complete" {
            stats.reports_delivered += 1;
        }
        stats.last_report = Some(
            json!({"requestId": plan.request_id, "planned": plan.planned,
            "sent": sent, "outcome": outcome}),
        );
    }

    /// The device-model variables monitoring owns, for `GetVariables` and the reports.
    pub(crate) fn device_variables(&self) -> Vec<DeviceVariable201> {
        self.0.lock().state.device_variables()
    }

    /// Whether the model lets this variable be monitored (`supportsMonitoring`).
    #[must_use]
    pub fn supports_monitoring(&self, component: &Value, variable: &Value) -> bool {
        let model = self.0.lock();
        model
            .state
            .variable(
                &component_from_json(component),
                &variable_from_json(variable),
            )
            .is_some_and(|entry| !entry.monitor_types.is_empty())
    }

    /// Workflow facts for scenario assertions; never a payload.
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let model = self.0.lock();
        let state = &model.state;
        let mut monitors: Vec<_> = state.monitors.iter().collect();
        monitors.sort_by_key(|monitor| monitor.id);
        json!({
            "base": state.base.as_str(),
            "level": state.level,
            "monitorCount": monitors.len(),
            "hardWired": state.hard_wired(),
            "monitorIds": monitors.iter().map(|monitor| monitor.id).collect::<Vec<_>>(),
            "monitorTypes": monitors.iter().map(|monitor| monitor.kind.as_str()).collect::<Vec<_>>(),
            "monitors": monitors.iter().map(|monitor| json!({
                "id": monitor.id, "type": monitor.kind.as_str(),
                "kind": monitor.origin.label(), "severity": monitor.severity,
                "value": monitor.value, "component": monitor.component.name,
                "variable": monitor.variable.name,
            })).collect::<Vec<_>>(),
            "variables": state.variables.iter().map(|entry| json!({
                "component": entry.component.name, "variable": entry.variable.name,
                "value": entry.value,
            })).collect::<Vec<_>>(),
            "eventsQueued": state.outbox.len(),
            "eventsEmitted": state.stats.events_emitted,
            "eventsAcknowledged": state.stats.events_acknowledged,
            "eventsRejected": state.stats.events_rejected,
            "eventsSuppressed": state.stats.events_suppressed,
            "lastEventOutcome": state.stats.last_event_outcome,
            "reportsDelivered": state.stats.reports_delivered,
            "reportFragmentsSent": state.stats.fragments_sent,
            "lastReport": state.stats.last_report,
        })
    }
}

impl State {
    /// One native CALL: `Err` is the CALLERROR code.
    fn call(
        &mut self,
        action: &str,
        payload: &Value,
        now: OffsetDateTime,
    ) -> Result<(Value, Option<ReportPlan201>), &'static str> {
        match action {
            "SetVariableMonitoring" => {
                let items = request::parse_set(
                    payload,
                    self.config.items_per_message_set,
                    self.config.bytes_per_message_set,
                )?;
                let mut results = self.set_monitoring(&items, now);
                match self.config.set_reply_fault {
                    SetReplyFault201::None => {}
                    // N04.FR.01: one result more than there were items.
                    SetReplyFault201::WrongResultCount => results.push(results[0].clone()),
                    // N04.FR.02: a component the request never named.
                    SetReplyFault201::MismatchedComponent => {
                        results[0]["component"] = json!({"name": "MismatchedComponent"});
                    }
                }
                Ok((json!({"setMonitoringResult": results}), None))
            }
            "ClearVariableMonitoring" => {
                let ids = request::parse_clear(
                    payload,
                    self.config.items_per_message_clear,
                    self.config.bytes_per_message_clear,
                )?;
                Ok((
                    json!({"clearMonitoringResult": self.clear_monitoring(&ids)}),
                    None,
                ))
            }
            "SetMonitoringBase" => {
                let base = request::parse_base(payload)?;
                let reply = match self.set_base(base, now) {
                    Ok(()) => json!({"status": "Accepted"}),
                    Err("NotSupported") => json!({"status": "NotSupported"}),
                    Err(reason) => {
                        json!({"status": "Rejected", "statusInfo": {"reasonCode": reason}})
                    }
                };
                Ok((reply, None))
            }
            "SetMonitoringLevel" => {
                let severity = request::parse_level(payload)?;
                let reply = if let Ok(level @ 0..=9) = u8::try_from(severity) {
                    self.level = level;
                    json!({"status": "Accepted"})
                } else {
                    // N05.FR.02: a severity out of range.
                    json!({"status": "Rejected", "statusInfo": {"reasonCode": "OutOfRange"}})
                };
                Ok((reply, None))
            }
            "GetMonitoringReport" => {
                let request = request::parse_report(payload)?;
                Ok(self.monitoring_report(&request, now))
            }
            _ => Err("NotImplemented"),
        }
    }
}
