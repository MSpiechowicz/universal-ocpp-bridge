use super::{ActionKind, RunFailure, StepDefinition, command, setup_failure};

#[allow(clippy::too_many_lines)] // Exhaustive per-action field validation reads best as one match.
pub(super) fn validate_fields(step: &StepDefinition) -> Result<(), RunFailure> {
    if step.start_delay_ms.checked_add(step.jitter_ms).is_none() {
        return Err(setup_failure(
            "invalid_step_bound",
            "step delay and jitter exceed the supported duration range",
        ));
    }
    if matches!(step.action, ActionKind::Wait | ActionKind::DelayLocalReply)
        != step.duration_ms.is_some()
    {
        return Err(setup_failure(
            "invalid_action_fields",
            "only wait actions require duration_ms",
        ));
    }
    if matches!(step.action, ActionKind::DelayLocalReply)
        && step
            .duration_ms
            .is_some_and(|duration| duration == 0 || duration > 30_000)
    {
        return Err(setup_failure(
            "invalid_native_delay",
            "native delay must be 1..=30000 milliseconds",
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
    let has_payload = is_charging_call
        || matches!(
            step.action,
            ActionKind::OfflineStart | ActionKind::OfflineStop
        );
    if has_payload != step.payload.is_some() {
        return Err(setup_failure(
            "invalid_action_payload",
            "charging calls require payload and other actions do not accept it",
        ));
    }
    validate_remote_start_binding(step)?;
    if step.use_current_timestamp
        && (!matches!(
            step.action,
            ActionKind::StartTransaction | ActionKind::StopTransaction
        ) || step
            .payload
            .as_ref()
            .is_some_and(|payload| payload.get("timestamp").is_some()))
    {
        return Err(setup_failure(
            "invalid_current_timestamp",
            "only transaction start/stop with an omitted timestamp accepts use_current_timestamp",
        ));
    }
    if step.use_active_transaction
        && (!matches!(step.action, ActionKind::StopTransaction)
            || step
                .payload
                .as_ref()
                .is_some_and(|payload| payload.get("transactionId").is_some()))
    {
        return Err(setup_failure(
            "invalid_active_transaction",
            "only stop_transaction with an omitted transactionId accepts use_active_transaction",
        ));
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
                | ActionKind::OfflineStart
                | ActionKind::OfflineStop
                | ActionKind::AssertLocalAuthorization
                | ActionKind::AwaitLocalAuthorization
                | ActionKind::AssertReservation
                | ActionKind::AwaitReservation
                | ActionKind::AssertFirmware
                | ActionKind::AwaitFirmware
                | ActionKind::AssertDiagnostics
                | ActionKind::AwaitDiagnostics
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

fn validate_remote_start_binding(step: &StepDefinition) -> Result<(), RunFailure> {
    if !step.use_awaited_remote_start_id {
        return Ok(());
    }
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
    Ok(())
}
