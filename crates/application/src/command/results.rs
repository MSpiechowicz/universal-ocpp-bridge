use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandResult,
    CommandValidationError, ContractVersion, ExternalCommand, UtcTimestamp,
};

pub(super) fn command_result<P>(
    command: &Command<P>,
    lifecycle: CommandLifecycle,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at,
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
        reservation_16: None,
        reservation_201: None,
        composite_schedule_201: None,
        charging_profiles_201: None,
        firmware_16: None,
        firmware_201: None,
        diagnostics_16: None,
        diagnostics_201: None,
    }
}

pub(super) fn rejected_external<P>(
    command: &ExternalCommand<P>,
    code: CommandErrorCode,
    detail: &str,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: uob_contracts::CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Rejected {
            error: CommandError {
                code,
                detail: Some(detail.to_owned()),
            },
        },
        recorded_at,
        observed_effects: Vec::new(),
        configuration: None,
        configuration_observations: Vec::new(),
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
        reservation_16: None,
        reservation_201: None,
        composite_schedule_201: None,
        charging_profiles_201: None,
        firmware_16: None,
        firmware_201: None,
        diagnostics_16: None,
        diagnostics_201: None,
    }
}

pub(super) fn validation_rejection<P>(
    command: &Command<P>,
    error: &CommandValidationError,
    recorded_at: UtcTimestamp,
) -> CommandResult {
    let code = match error {
        CommandValidationError::Expired => CommandErrorCode::Expired,
        CommandValidationError::UnsupportedOperation(_) => CommandErrorCode::UnsupportedOperation,
    };
    command_result(
        command,
        CommandLifecycle::Rejected {
            error: CommandError {
                code,
                detail: Some(error.to_string()),
            },
        },
        recorded_at,
    )
}
