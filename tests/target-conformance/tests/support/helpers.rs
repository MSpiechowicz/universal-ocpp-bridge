use std::future::Future;

use time::OffsetDateTime;
use tokio::time::Duration;
use uob_contracts::{
    CommandLifecycle, CommandResult, CommandReturnRoute, ContractVersion, ExternalCommand,
    UtcTimestamp,
};

pub(crate) fn timestamp(minute: i64) -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(minute))
}

pub(crate) async fn timeout<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(1), future)
        .await
        .expect("scenario timed out")
}

pub(crate) fn text<T, E: std::fmt::Debug>(
    constructor: impl FnOnce(String) -> Result<T, E>,
    value: impl Into<String>,
) -> T {
    constructor(value.into()).expect("valid fixture text")
}

pub(crate) fn result_for(command: &ExternalCommand<()>) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Admitted,
        recorded_at: timestamp(1),
        observed_effects: vec![],
        configuration: None,
        configuration_observations: vec![],
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
    }
}
