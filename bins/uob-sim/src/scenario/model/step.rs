use super::{ActionKind, RunFailure, StepDefinition, command, setup_failure};

pub(super) fn validate_fields(step: &StepDefinition) -> Result<(), RunFailure> {
    if step.start_delay_ms.checked_add(step.jitter_ms).is_none() {
        return Err(setup_failure(
            "invalid_step_bound",
            "step delay and jitter exceed the supported duration range",
        ));
    }
    if matches!(step.action, ActionKind::Wait) != step.duration_ms.is_some() {
        return Err(setup_failure(
            "invalid_action_fields",
            "only wait actions require duration_ms",
        ));
    }
    command::validate_fields(step)?;
    let is_charging_call = matches!(
        step.action,
        ActionKind::Boot
            | ActionKind::Authorize
            | ActionKind::Status
            | ActionKind::StartTransaction
            | ActionKind::MeterValues
            | ActionKind::StopTransaction
    );
    if is_charging_call != step.payload.is_some() {
        return Err(setup_failure(
            "invalid_action_payload",
            "charging calls require payload and other actions do not accept it",
        ));
    }
    if step.use_awaited_remote_start_id {
        if !matches!(step.action, ActionKind::StartTransaction) {
            return Err(setup_failure(
                "invalid_remote_start_binding",
                "only start_transaction accepts use_awaited_remote_start_id",
            ));
        }
        if step
            .payload
            .as_ref()
            .is_some_and(|payload| payload.pointer("/transactionInfo/remoteStartId").is_some())
        {
            return Err(setup_failure(
                "invalid_remote_start_binding",
                "bound start payload must omit transactionInfo.remoteStartId",
            ));
        }
    }
    if is_charging_call != step.fixture_id.is_some() {
        return Err(setup_failure(
            "missing_wire_fixture",
            "charging calls require an independently authored fixture_id",
        ));
    }
    if step.expect_response.is_some()
        && !matches!(
            step.action,
            ActionKind::Boot
                | ActionKind::Authorize
                | ActionKind::Status
                | ActionKind::StartTransaction
                | ActionKind::MeterValues
                | ActionKind::StopTransaction
                | ActionKind::AwaitRemoteStart
                | ActionKind::AwaitRemoteStop
        )
    {
        return Err(setup_failure(
            "invalid_expected_response",
            "only protocol actions accept expect_response",
        ));
    }
    if let Some(event) = &step.expect_event
        && event != step.action.event()
    {
        return Err(setup_failure(
            "unsupported_expected_event",
            "expected event is not produced by the selected action",
        ));
    }
    if let Some(message) = &step.expect_message
        && !step.action.accepts_message(message)
    {
        return Err(setup_failure(
            "unsupported_expected_message",
            "expected message does not match the selected action",
        ));
    }
    command::validate_fault(step)?;
    Ok(())
}
