//! Defense in depth: never serialize raw OCPP 1.6 configuration values into a command row.
use serde::Serialize;
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    CONFIGURATION_CHANGE_REFERENCE_SCHEMA, Command, CommandOperation, ConfigurationChangeReference,
    ProtocolEdition,
};

pub(super) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    if operation.protocol != ProtocolEdition::Ocpp16j
        || !matches!(
            operation.action.as_str(),
            "ChangeConfiguration" | "GetConfiguration"
        )
    {
        return Ok(());
    }
    let invalid = || {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "invalid configuration payload",
        )
    };
    let payload = serde_json::to_value(&operation.payload).map_err(|_| invalid())?;
    let fields = payload.as_object().ok_or_else(invalid)?;
    let valid = match operation.action.as_str() {
        "ChangeConfiguration" => {
            operation.payload_schema.as_str() == CONFIGURATION_CHANGE_REFERENCE_SCHEMA
                && fields.len() == 2
                && fields
                    .get("key")
                    .and_then(serde_json::Value::as_str)
                    .zip(
                        fields
                            .get("valueReference")
                            .and_then(serde_json::Value::as_str),
                    )
                    .is_some_and(|(key, reference)| {
                        ConfigurationChangeReference::valid_parts(key, reference)
                    })
        }
        "GetConfiguration" => {
            operation.payload_schema.as_str() == "urn:OCPP:1.6:2019:12:GetConfigurationRequest"
                && (fields.is_empty()
                    || (fields.len() == 1
                        && fields
                            .get("key")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|keys| {
                                keys.len() <= 256
                                    && keys.iter().all(|key| {
                                        key.as_str().is_some_and(|key| key.chars().count() <= 50)
                                    })
                            })))
        }
        _ => unreachable!(),
    };
    if valid { Ok(()) } else { Err(invalid()) }
}
