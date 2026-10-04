//! Independent reference-only storage validation and immutable native-result correlation.
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    Command, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult, ContractVersion,
    LocalAuthorizationResult201, NativeProtocolReference, ProtocolEdition,
    SEND_LOCAL_LIST_REFERENCE_SCHEMA_201, SendLocalListReference201,
};
fn invalid() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "invalid protected local authorization identity or evidence",
    )
}
pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    if !["GetLocalListVersion", "SendLocalList", "ClearCache"].contains(&operation.action.as_str())
    {
        return Ok(());
    }
    if operation.protocol == ProtocolEdition::Ocpp16j {
        return Ok(());
    }
    if operation.protocol != ProtocolEdition::Ocpp201
        || command.resource.resource.is_some()
        || !matches!(
            command.resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
        )
    {
        return Err(invalid());
    }
    let payload = serde_json::to_value(&operation.payload).map_err(|_| invalid())?;
    let valid = match operation.action.as_str() {
        "SendLocalList" => {
            operation.payload_schema.as_str() == SEND_LOCAL_LIST_REFERENCE_SCHEMA_201
                && serde_json::from_value::<SendLocalListReference201>(payload)
                    .is_ok_and(|request| request.valid())
        }
        "GetLocalListVersion" => {
            operation.payload_schema.as_str() == "urn:OCPP:Cp:2:2020:3:GetLocalListVersionRequest"
                && payload.as_object().is_some_and(serde_json::Map::is_empty)
        }
        "ClearCache" => {
            operation.payload_schema.as_str() == "urn:OCPP:Cp:2:2020:3:ClearCacheRequest"
                && payload.as_object().is_some_and(serde_json::Map::is_empty)
        }
        _ => false,
    };
    if valid { Ok(()) } else { Err(invalid()) }
}
pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.local_authorization_201 else {
        return Ok(());
    };
    if result.schema_version != ContractVersion::V1_LOCAL_AUTHORIZATION_201
        || !matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == evidence.accepted())
        || result.configuration.is_some()
        || !result.configuration_observations.is_empty()
        || result.configuration_201.is_some()
        || result.charging_profile_16.is_some()
        || result.charging_profile_201.is_some()
        || result.composite_schedule_16.is_some()
        || result.device_model_201.is_some()
        || result.trigger_observation.is_some()
        || result.trigger_observation_201.is_some()
        || result.local_authorization_16.is_some()
    {
        return Err(invalid());
    }
    if let CommandLifecycle::ProtocolResponse {
        error: Some(error), ..
    } = &result.lifecycle
        && (evidence.accepted()
            || error.code != CommandErrorCode::ProtocolRejected
            || error.detail.is_some())
    {
        return Err(invalid());
    }
    if match evidence {
        LocalAuthorizationResult201::GetLocalListVersion { version_number } => *version_number < 0,
        LocalAuthorizationResult201::SendLocalList { version_number, .. } => *version_number <= 0,
        LocalAuthorizationResult201::ClearCache { .. } => false,
    } {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn correlate(
    command: &Command<Value>,
    result: &CommandResult,
) -> Result<(), StorageError> {
    validate_command(command)?;
    validate_result(result)?;
    if command.resource != result.resource
        || command.return_route() != result.return_route
        || command.correlation_id != result.correlation_id
    {
        return Err(invalid());
    }
    let Some(evidence) = &result.local_authorization_201 else {
        return Ok(());
    };
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Err(invalid());
    };
    if operation.protocol != ProtocolEdition::Ocpp201 {
        return Err(invalid());
    }
    let valid = match evidence {
        LocalAuthorizationResult201::GetLocalListVersion { .. } => {
            operation.action.as_str() == "GetLocalListVersion"
        }
        LocalAuthorizationResult201::ClearCache { .. } => operation.action.as_str() == "ClearCache",
        LocalAuthorizationResult201::SendLocalList {
            version_number,
            update_type,
            ..
        } => {
            operation.action.as_str() == "SendLocalList"
                && serde_json::from_value::<SendLocalListReference201>(operation.payload.clone())
                    .is_ok_and(|request| {
                        request.version_number == *version_number
                            && request.update_type == *update_type
                    })
        }
    };
    if valid { Ok(()) } else { Err(invalid()) }
}
pub(crate) fn validate_stored(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    if result.local_authorization_201.is_none() {
        return Ok(());
    }
    let payload = connection
        .query_row(
            "SELECT payload FROM commands WHERE request_id = ?1",
            [result.return_route.request_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(crate::configuration::unavailable)?
        .ok_or_else(invalid)?;
    let command = serde_json::from_str::<Command<Value>>(&payload).map_err(|_| invalid())?;
    correlate(&command, result)
}
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    if let Some(evidence) = &previous.local_authorization_201 {
        if incoming
            .local_authorization_201
            .as_ref()
            .is_some_and(|new| new != evidence)
        {
            return Err(invalid());
        }
        incoming.local_authorization_201 = Some(evidence.clone());
        incoming.schema_version = previous.schema_version;
        incoming.lifecycle = previous.lifecycle.clone();
        incoming.recorded_at = previous.recorded_at;
    }
    Ok(())
}
