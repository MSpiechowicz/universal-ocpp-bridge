use super::execution::failure;
use super::{ActionKind, RunFailure, StepDefinition};
use crate::ProtocolClient;

pub(super) async fn observe(
    step: &StepDefinition,
    client: Option<&dyn ProtocolClient>,
) -> Result<String, RunFailure> {
    let expected = step.expect_response.as_ref().ok_or_else(|| {
        failure(
            "reservation_assertion_missing",
            "reservation assertion requires expected response",
        )
    })?;
    if !expected.as_object().is_some_and(|fields| {
        !fields.is_empty()
            && fields.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "stateAvailable" | "activeReservations" | "revision" | "reservations"
                )
            })
    }) {
        return Err(failure(
            "reservation_assertion_invalid",
            "reservation assertion accepts only safe workflow metadata",
        ));
    }
    let handle = client
        .and_then(ProtocolClient::reservation16)
        .ok_or_else(|| {
            failure(
                "reservation_state_missing",
                "durable native reservation state is unavailable",
            )
        })?;
    loop {
        let actual = handle.snapshot();
        if expected
            .as_object()
            .expect("validated fields")
            .iter()
            .all(|(key, value)| actual.get(key) == Some(value))
        {
            return Ok(actual.to_string());
        }
        if matches!(step.action, ActionKind::AssertReservation) {
            return Err(failure(
                "reservation_assertion_failed",
                "native reservation state differs from expectation",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
