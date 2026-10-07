use super::execution::failure;
use super::{ActionKind, RunFailure, StepDefinition};
use crate::ProtocolClient;

const SAFE_KEYS: &[&str] = &[
    "stateAvailable",
    "mode",
    "active",
    "requestId",
    "lastStatus",
    "statuses",
    "installedVersion",
    "reboots",
    "pendingStatuses",
    "cancelled",
];

/// Compares only safe OCPP 1.6 or 2.0.1 firmware workflow metadata; `await_firmware` polls until the step timeout.
pub(super) async fn observe(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let expected = step.expect_response.as_ref().ok_or_else(|| {
        failure(
            "firmware_assertion_missing",
            "firmware assertion requires expected response",
        )
    })?;
    let Some(fields) = expected.as_object().filter(|fields| {
        !fields.is_empty() && fields.keys().all(|key| SAFE_KEYS.contains(&key.as_str()))
    }) else {
        return Err(failure(
            "firmware_assertion_invalid",
            "firmware assertion accepts only safe workflow metadata",
        ));
    };
    // A station carries at most one edition's firmware model.
    let snapshot: Box<dyn Fn() -> serde_json::Value + Send + Sync> =
        if let Some(handle) = client.and_then(ProtocolClient::firmware16) {
            Box::new(move || handle.snapshot())
        } else {
            let handle = client
                .and_then(ProtocolClient::firmware201)
                .ok_or_else(|| {
                    failure(
                        "firmware_state_missing",
                        "durable native firmware state is unavailable",
                    )
                })?;
            Box::new(move || handle.snapshot())
        };
    loop {
        let actual = snapshot();
        if fields
            .iter()
            .all(|(key, value)| actual.get(key) == Some(value))
        {
            return Ok(actual.to_string());
        }
        if matches!(step.action, ActionKind::AssertFirmware) {
            return Err(failure(
                "firmware_assertion_failed",
                "native firmware state differs from expectation",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
