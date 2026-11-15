//! Scenario steps for the OCPP 2.0.1 monitoring model: change a monitored value, raise an event
//! burst, and assert or await its workflow facts.
use super::execution::failure;
use super::{ActionKind, FailureCategory, RunFailure, StepDefinition};
use crate::ProtocolClient;
use serde_json::Value;

const SAFE_KEYS: &[&str] = &[
    "base",
    "level",
    "monitorCount",
    "hardWired",
    "monitorIds",
    "monitorTypes",
    "monitors",
    "variables",
    "eventsQueued",
    "eventsEmitted",
    "eventsAcknowledged",
    "eventsRejected",
    "eventsSuppressed",
    "lastEventOutcome",
    "reportsDelivered",
    "reportFragmentsSent",
    "lastReport",
];
const BURST_LIMIT: u64 = 1000;

fn invalid(message: &'static str) -> RunFailure {
    RunFailure::new(
        FailureCategory::Setup,
        "invalid_monitoring_payload",
        message,
    )
}

fn named(value: Option<&Value>) -> bool {
    value
        .and_then(|value| value.get("name"))
        .is_some_and(Value::is_string)
}

/// Setup-time shape check of the payload of the two acting steps.
pub(super) fn validate_payload(step: &StepDefinition) -> Result<(), RunFailure> {
    let allowed: &[&str] = match step.action {
        ActionKind::SetMonitoredValue => &["component", "variable", "value"],
        ActionKind::EmitMonitoringEvents => &["component", "variable", "count", "monitorId"],
        _ => return Ok(()),
    };
    let payload = step
        .payload
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("monitoring step payload must be a table"))?;
    if payload.keys().any(|key| !allowed.contains(&key.as_str()))
        || !named(payload.get("component"))
        || !named(payload.get("variable"))
    {
        return Err(invalid(
            "monitoring step payload needs a named component and variable",
        ));
    }
    match step.action {
        ActionKind::SetMonitoredValue
            if !payload
                .get("value")
                .is_some_and(|value| value.is_string() || value.is_number()) =>
        {
            Err(invalid(
                "set_monitored_value needs a string or number value",
            ))
        }
        ActionKind::EmitMonitoringEvents
            if !payload
                .get("count")
                .and_then(Value::as_u64)
                .is_some_and(|count| (1..=BURST_LIMIT).contains(&count)) =>
        {
            Err(invalid("emit_monitoring_events needs a count of 1..=1000"))
        }
        _ => Ok(()),
    }
}

fn handle(
    client: Option<&dyn ProtocolClient>,
) -> Result<crate::monitoring201::Monitoring201Handle, RunFailure> {
    client
        .and_then(ProtocolClient::monitoring201)
        .ok_or_else(|| {
            failure(
                "monitoring_state_missing",
                "native monitoring state is unavailable",
            )
        })
}

/// `set_monitored_value` and `emit_monitoring_events`; the detail is the number of events queued.
pub(super) fn act(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let handle = handle(client)?;
    let payload = step.payload.as_ref().expect("validated monitoring payload");
    let queued = if matches!(step.action, ActionKind::SetMonitoredValue) {
        let value = match &payload["value"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        handle.set_value(&payload["component"], &payload["variable"], &value)
    } else {
        let count = usize::try_from(payload["count"].as_u64().unwrap_or(0)).unwrap_or(0);
        let monitor = payload
            .get("monitorId")
            .and_then(Value::as_i64)
            .and_then(|id| i32::try_from(id).ok());
        handle.emit_events(&payload["component"], &payload["variable"], count, monitor)
    };
    queued
        .map(|count| format!("events={count}"))
        .map_err(|code| failure(code, "monitoring step was refused by the native model"))
}

/// Compares only safe monitoring workflow facts; `await_monitoring` polls until the step timeout.
pub(super) async fn observe(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let expected = step.expect_response.as_ref().ok_or_else(|| {
        failure(
            "monitoring_assertion_missing",
            "monitoring assertion requires expected response",
        )
    })?;
    let Some(fields) = expected.as_object().filter(|fields| {
        !fields.is_empty() && fields.keys().all(|key| SAFE_KEYS.contains(&key.as_str()))
    }) else {
        return Err(failure(
            "monitoring_assertion_invalid",
            "monitoring assertion accepts only safe workflow metadata",
        ));
    };
    let handle = handle(client)?;
    loop {
        let actual = handle.snapshot();
        if fields
            .iter()
            .all(|(key, value)| actual.get(key) == Some(value))
        {
            return Ok(actual.to_string());
        }
        if matches!(step.action, ActionKind::AssertMonitoring) {
            return Err(failure(
                "monitoring_assertion_failed",
                "native monitoring state differs from expectation",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
