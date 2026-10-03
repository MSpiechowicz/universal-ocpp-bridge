//! Reject raw write material before admission, and correlate value-free evidence on every read.
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    CONFIGURATION_ITEMS_LIMIT_201, CanonicalResource, Command, CommandErrorCode, CommandLifecycle,
    CommandOperation, CommandResult, ConfigurationResult201, ContractVersion,
    DeviceAttributeType201, DeviceComponent201, DeviceVariable201, NativeProtocolReference,
    ProtocolEdition, ResourceRef, SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201,
    SET_VARIABLES_REFERENCE_SCHEMA_201, SetNetworkProfileReference201, SetNetworkProfileStatus201,
    SetVariablesReference201, valid_configuration_reference_201,
};
fn invalid() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "invalid protected configuration identity or evidence",
    )
}
fn key(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
    attribute: DeviceAttributeType201,
) -> String {
    let fold = caseless::default_case_fold_str;
    serde_json::to_string(&(
        fold(&component.name),
        component.instance.as_deref().map(fold),
        component
            .evse
            .as_ref()
            .map(|evse| (evse.id, evse.connector_id)),
        fold(&variable.name),
        variable.instance.as_deref().map(fold),
        attribute,
    ))
    .expect("bounded identity")
}
fn contained(resource: &ResourceRef, component: &DeviceComponent201) -> bool {
    match (&resource.resource, resource.native_protocol_reference) {
        (None, None) => true,
        (
            Some(CanonicalResource::Evse {
                connector_id: canonical,
                ..
            }),
            Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id,
            }),
        ) => {
            canonical.is_some() == connector_id.is_some()
                && component.evse.as_ref().is_some_and(|evse| {
                    u32::try_from(evse.id).ok() == Some(evse_id)
                        && connector_id.is_none_or(|id| {
                            evse.connector_id.and_then(|v| u32::try_from(v).ok()) == Some(id)
                        })
                })
        }
        _ => false,
    }
}
pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    if operation.protocol != ProtocolEdition::Ocpp201
        || !["SetVariables", "SetNetworkProfile"].contains(&operation.action.as_str())
    {
        return Ok(());
    }
    let payload = serde_json::to_value(&operation.payload).map_err(|_| invalid())?;
    match operation.action.as_str() {
        "SetVariables" => {
            if operation.payload_schema.as_str() != SET_VARIABLES_REFERENCE_SCHEMA_201 {
                return Err(invalid());
            }
            let request: SetVariablesReference201 =
                serde_json::from_value(payload).map_err(|_| invalid())?;
            if request.set_variable_data.is_empty()
                || request.set_variable_data.len() > CONFIGURATION_ITEMS_LIMIT_201
            {
                return Err(invalid());
            }
            let mut identities = BTreeSet::new();
            for entry in request.set_variable_data {
                let bounded = |value: &str| value.chars().count() <= 50;
                if !valid_configuration_reference_201(&entry.value_reference)
                    || !contained(&command.resource, &entry.component)
                    || !bounded(&entry.component.name)
                    || !bounded(&entry.variable.name)
                    || !entry.component.instance.as_deref().is_none_or(bounded)
                    || !entry.variable.instance.as_deref().is_none_or(bounded)
                    || entry.component.evse.as_ref().is_some_and(|evse| {
                        evse.id <= 0 || evse.connector_id.is_some_and(|id| id <= 0)
                    })
                    || !identities.insert(key(
                        &entry.component,
                        &entry.variable,
                        entry.attribute_type.unwrap_or_default(),
                    ))
                {
                    return Err(invalid());
                }
            }
        }
        "SetNetworkProfile" => {
            if operation.payload_schema.as_str() != SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201
                || command.resource.resource.is_some()
                || command.resource.native_protocol_reference.is_some()
            {
                return Err(invalid());
            }
            let request: SetNetworkProfileReference201 =
                serde_json::from_value(payload).map_err(|_| invalid())?;
            if !valid_configuration_reference_201(&request.profile_reference) {
                return Err(invalid());
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.configuration_201 else {
        return Ok(());
    };
    if result.schema_version != ContractVersion::V1_CONFIGURATION_201
        || !matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if accepted == evidence.accepted())
    {
        return Err(invalid());
    }
    if result.configuration.is_some()
        || !result.configuration_observations.is_empty()
        || result.device_model_201.is_some()
        || result.charging_profile_16.is_some()
        || result.charging_profile_201.is_some()
        || result.composite_schedule_16.is_some()
        || result.trigger_observation.is_some()
        || result.trigger_observation_201.is_some()
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
    match evidence {
        ConfigurationResult201::SetVariables { variables } => {
            if variables.is_empty() || variables.len() > CONFIGURATION_ITEMS_LIMIT_201 {
                return Err(invalid());
            }
            let mut identities = BTreeSet::new();
            if variables.iter().any(|entry| {
                !identities.insert(key(&entry.component, &entry.variable, entry.attribute_type))
            }) {
                return Err(invalid());
            }
        }
        ConfigurationResult201::SetNetworkProfile { status, staged, .. } => {
            if *staged != (*status == SetNetworkProfileStatus201::Accepted) {
                return Err(invalid());
            }
        }
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
    let Some(evidence) = &result.configuration_201 else {
        return Ok(());
    };
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Err(invalid());
    };
    if operation.protocol != ProtocolEdition::Ocpp201 {
        return Err(invalid());
    }
    match evidence {
        ConfigurationResult201::SetVariables { variables }
            if operation.action.as_str() == "SetVariables" =>
        {
            let request =
                SetVariablesReference201::deserialize(&operation.payload).map_err(|_| invalid())?;
            if request.set_variable_data.len() != variables.len()
                || request
                    .set_variable_data
                    .iter()
                    .zip(variables)
                    .any(|(request, result)| {
                        request.component != result.component
                            || request.variable != result.variable
                            || request.attribute_type.unwrap_or_default() != result.attribute_type
                    })
            {
                return Err(invalid());
            }
        }
        ConfigurationResult201::SetNetworkProfile {
            configuration_slot, ..
        } if operation.action.as_str() == "SetNetworkProfile" => {
            let request = SetNetworkProfileReference201::deserialize(&operation.payload)
                .map_err(|_| invalid())?;
            if *configuration_slot != request.configuration_slot {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
pub(crate) fn validate_stored(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    if result.configuration_201.is_none() {
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
    let command: Command<Value> = serde_json::from_str(&payload).map_err(|_| invalid())?;
    correlate(&command, result)
}
