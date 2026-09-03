use super::{ActionKind, FaultKind, RunFailure, StepDefinition, setup_failure};

pub(super) fn validate_fields(step: &StepDefinition) -> Result<(), RunFailure> {
    if step
        .expect_failure
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(setup_failure(
            "invalid_expected_failure",
            "expect_failure must name a stable failure code",
        ));
    }
    let is_remote = matches!(
        step.action,
        ActionKind::AwaitRemoteStart | ActionKind::AwaitRemoteStop
    );
    let is_reconcile = matches!(step.action, ActionKind::ReconcileCommand);
    let has_command_fields = step.request_id.is_some()
        || step.delivery_id.is_some()
        || step.expires_at_ms.is_some()
        || step.execute_at_ms.is_some();
    if has_command_fields && !is_remote && !is_reconcile {
        return Err(setup_failure(
            "invalid_command_fields",
            "command identity and deadline fields require a remote-command action",
        ));
    }
    if is_reconcile {
        if step
            .request_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
            || step.delivery_id.is_some()
            || step.expires_at_ms.is_some()
            || step.execute_at_ms.is_some()
        {
            return Err(setup_failure(
                "invalid_command_fields",
                "reconcile_command requires only a nonempty request_id",
            ));
        }
        return Ok(());
    }
    if is_remote
        && has_command_fields
        && (step
            .request_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
            || step
                .delivery_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            || step.execute_at_ms.is_none())
    {
        return Err(setup_failure(
            "invalid_command_fields",
            "tracked remote commands require request_id, delivery_id, and execute_at_ms",
        ));
    }
    Ok(())
}

pub(super) fn validate_fault(step: &StepDefinition) -> Result<(), RunFailure> {
    let Some(fault) = &step.fault else {
        return Ok(());
    };
    if fault.probability_percent == 0 || fault.probability_percent > 100 {
        return Err(setup_failure(
            "invalid_fault_probability",
            "fault probability_percent must be between 1 and 100",
        ));
    }
    if matches!(
        fault.kind,
        FaultKind::ResponseDelay | FaultKind::OutOfOrderResponse
    ) && fault.delay_ms == 0
    {
        return Err(setup_failure(
            "invalid_fault_delay",
            "response delay and out-of-order controls require a nonzero delay_ms",
        ));
    }
    if !matches!(
        step.action,
        ActionKind::Heartbeat | ActionKind::AwaitRemoteStart | ActionKind::AwaitRemoteStop
    ) {
        return Err(setup_failure(
            "invalid_fault_action",
            "response fault controls are unsupported for this action",
        ));
    }
    if !matches!(step.action, ActionKind::Heartbeat)
        && !matches!(fault.kind, FaultKind::MissingResponse)
    {
        return Err(setup_failure(
            "invalid_fault_action",
            "remote command scenarios support only missing_response faults",
        ));
    }
    if matches!(
        step.action,
        ActionKind::AwaitRemoteStart | ActionKind::AwaitRemoteStop
    ) && step.request_id.is_none()
    {
        return Err(setup_failure(
            "invalid_fault_action",
            "remote missing_response faults require tracked command metadata",
        ));
    }
    Ok(())
}
