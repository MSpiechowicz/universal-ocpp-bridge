use super::execution::failure;
use super::{ActionKind, RunFailure, StepDefinition};
use crate::ProtocolClient;

const SAFE_KEYS: &[&str] = &[
    "stateAvailable",
    "legacy",
    "securityLog",
    "active",
    "kind",
    "requestId",
    "fileName",
    "attempts",
    "lastStatus",
    "statuses",
    "uploads",
    "pendingStatuses",
    "cancelled",
];

/// Compares only safe OCPP 1.6 log-upload workflow metadata; `await_diagnostics` polls until the
/// step timeout.
pub(super) async fn observe(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let expected = step.expect_response.as_ref().ok_or_else(|| {
        failure(
            "diagnostics_assertion_missing",
            "diagnostics assertion requires expected response",
        )
    })?;
    let Some(fields) = expected.as_object().filter(|fields| {
        !fields.is_empty() && fields.keys().all(|key| SAFE_KEYS.contains(&key.as_str()))
    }) else {
        return Err(failure(
            "diagnostics_assertion_invalid",
            "diagnostics assertion accepts only safe workflow metadata",
        ));
    };
    let handle = client
        .and_then(ProtocolClient::diagnostics16)
        .ok_or_else(|| {
            failure(
                "diagnostics_state_missing",
                "durable native diagnostics state is unavailable",
            )
        })?;
    loop {
        let actual = handle.snapshot();
        if fields
            .iter()
            .all(|(key, value)| actual.get(key) == Some(value))
        {
            return Ok(actual.to_string());
        }
        if matches!(step.action, ActionKind::AssertDiagnostics) {
            return Err(failure(
                "diagnostics_assertion_failed",
                "native diagnostics state differs from expectation",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
