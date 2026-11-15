//! Scenario steps for the OCPP 1.6 security model: queue a `SecurityEventNotification` and
//! assert or await the outbox's workflow facts.
use super::execution::failure;
use super::{ActionKind, FailureCategory, RunFailure, StepDefinition};
use crate::ProtocolClient;
use serde_json::Value;

const SAFE_KEYS: &[&str] = &[
    "stateAvailable",
    "pendingEvents",
    "eventsEmitted",
    "eventsAcknowledged",
    "eventsRejected",
    "duplicatesSent",
    "lastAcknowledgedType",
    "triggersReceived",
    "lastTrigger",
];
const PAYLOAD_KEYS: &[&str] = &["type", "timestamp", "techInfo"];

fn invalid(code: &'static str, message: &'static str) -> RunFailure {
    RunFailure::new(FailureCategory::Setup, code, message)
}

fn text<'a>(
    payload: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, RunFailure> {
    match payload.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(invalid(
            "invalid_security_payload",
            "security event fields must be strings",
        )),
    }
}

/// Setup-time check of an `emit_security_event` payload against the pinned
/// `SecurityEventNotification.req` bounds.
pub(super) fn validate_payload(step: &StepDefinition) -> Result<(), RunFailure> {
    if !matches!(step.action, ActionKind::EmitSecurityEvent) {
        return Ok(());
    }
    let payload = step
        .payload
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| {
            invalid(
                "invalid_security_payload",
                "security step payload must be a table",
            )
        })?;
    if payload
        .keys()
        .any(|key| !PAYLOAD_KEYS.contains(&key.as_str()))
    {
        return Err(invalid(
            "invalid_security_payload",
            "security step payload accepts only type, timestamp and techInfo",
        ));
    }
    let kind = text(payload, "type")?.ok_or_else(|| {
        invalid(
            "invalid_security_payload",
            "security step payload requires a type",
        )
    })?;
    let timestamp = text(payload, "timestamp")?.unwrap_or("1970-01-01T00:00:00Z");
    crate::security16::validate_event(kind, timestamp, text(payload, "techInfo")?).map_err(|code| {
        invalid(
            code,
            "security event violates the pinned SecurityEventNotification schema",
        )
    })
}

fn handle(
    client: Option<&dyn ProtocolClient>,
) -> Result<crate::security16::SecurityHandle, RunFailure> {
    client.and_then(ProtocolClient::security16).ok_or_else(|| {
        failure(
            "security_state_missing",
            "native security state is unavailable",
        )
    })
}

/// `emit_security_event`: queue the event durably; the detail is its outbox identity.
pub(super) fn act(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let handle = handle(client)?;
    let payload = step.payload.as_ref().expect("validated security payload");
    let field = |key: &str| payload.get(key).and_then(Value::as_str);
    handle
        .emit(
            field("type").expect("validated security type"),
            field("timestamp"),
            field("techInfo"),
            time::OffsetDateTime::now_utc(),
        )
        .map(|id| format!("event={id}"))
        .map_err(|code| failure(code, "security event was refused by the native model"))
}

/// Compares only safe security workflow facts; `await_security` polls until the step timeout.
pub(super) async fn observe(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let expected = step.expect_response.as_ref().ok_or_else(|| {
        failure(
            "security_assertion_missing",
            "security assertion requires expected response",
        )
    })?;
    let Some(fields) = expected.as_object().filter(|fields| {
        !fields.is_empty() && fields.keys().all(|key| SAFE_KEYS.contains(&key.as_str()))
    }) else {
        return Err(failure(
            "security_assertion_invalid",
            "security assertion accepts only safe workflow metadata",
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
        if matches!(step.action, ActionKind::AssertSecurity) {
            return Err(failure(
                "security_assertion_failed",
                "native security state differs from expectation",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
