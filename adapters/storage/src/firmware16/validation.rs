use super::{conflict, transitions::evidence};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use uob_application::{FirmwareJobMutation16, FirmwareVariant16, StorageError, StorageErrorCode};
use uob_contracts::{
    Command, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult, ContractVersion,
    FirmwareResult16, NativeProtocolReference, ProtocolEdition,
    SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16, SignedUpdateFirmwareReference16,
    UPDATE_FIRMWARE_REFERENCE_SCHEMA_16, UpdateFirmwareReference16,
};

const ACTIONS: [&str; 2] = ["UpdateFirmware", "SignedUpdateFirmware"];

/// Immutable native identity and artifact of a stored firmware command.
fn request(command: &Command<Value>) -> Option<(FirmwareVariant16, String)> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    match operation.action.as_str() {
        "UpdateFirmware"
            if operation.payload_schema.as_str() == UPDATE_FIRMWARE_REFERENCE_SCHEMA_16 =>
        {
            serde_json::from_value::<UpdateFirmwareReference16>(operation.payload.clone())
                .ok()
                .map(|request| (FirmwareVariant16::Legacy, request.artifact_reference))
        }
        "SignedUpdateFirmware"
            if operation.payload_schema.as_str() == SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16 =>
        {
            serde_json::from_value::<SignedUpdateFirmwareReference16>(operation.payload.clone())
                .ok()
                .map(|request| {
                    (
                        FirmwareVariant16::Signed {
                            request_id: request.request_id,
                        },
                        request.artifact_reference,
                    )
                })
        }
        _ => None,
    }
}

pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    // OCPP 2.0.1 `UpdateFirmware` is validated by its own edition module.
    if !ACTIONS.contains(&operation.action.as_str())
        || (operation.protocol == ProtocolEdition::Ocpp201
            && operation.action.as_str() == "UpdateFirmware")
    {
        return Ok(());
    }
    let envelope: Command<Value> = serde_json::to_value(command)
        .and_then(serde_json::from_value)
        .map_err(|_| conflict("invalid firmware command"))?;
    if operation.protocol != ProtocolEdition::Ocpp16j
        || envelope.resource.resource.is_some()
        || !matches!(
            envelope.resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
        )
        || request(&envelope).is_none()
    {
        return Err(conflict("invalid protected firmware command"));
    }
    Ok(())
}

fn owner(connection: &Connection, request_id: &str) -> Result<Command<Value>, StorageError> {
    let payload: String = connection
        .query_row(
            "SELECT payload FROM commands WHERE request_id=?1",
            [request_id],
            |row| row.get(0),
        )
        .map_err(crate::configuration::unavailable)?;
    let command: Command<Value> =
        serde_json::from_str(&payload).map_err(|_| conflict("invalid firmware command owner"))?;
    validate_command(&command)?;
    Ok(command)
}

pub(crate) fn validate_mutation(
    connection: &Connection,
    mutation: &FirmwareJobMutation16,
) -> Result<(), StorageError> {
    let command = owner(connection, mutation.request_id.as_str())?;
    if command.resource.bridge_id != mutation.station.bridge_id
        || command.resource.station_id != mutation.station.station_id
        || mutation.station.resource.is_some()
        || command.admitted_at != mutation.admitted_at
        || request(&command) != Some((mutation.variant, mutation.artifact_reference.clone()))
    {
        return Err(conflict("firmware job changed immutable request"));
    }
    Ok(())
}

pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.firmware_16 else {
        return Ok(());
    };
    let native = evidence.reply().is_some();
    if result.schema_version != ContractVersion::V1_FIRMWARE_16
        || result.configuration.is_some()
        || !result.configuration_observations.is_empty()
        || result.configuration_201.is_some()
        || result.composite_schedule_16.is_some()
        || result.composite_schedule_201.is_some()
        || result.device_model_201.is_some()
        || result.charging_profile_16.is_some()
        || result.charging_profile_201.is_some()
        || result.charging_profiles_201.is_some()
        || result.trigger_observation.is_some()
        || result.trigger_observation_201.is_some()
        || result.local_authorization_16.is_some()
        || result.local_authorization_201.is_some()
        || result.reservation_16.is_some()
        || result.reservation_201.is_some()
        || result.firmware_201.is_some()
        || result.diagnostics_16.is_some()
        || native != matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { .. })
        || evidence.artifact().is_some_and(|artifact| {
            !uob_contracts::valid_firmware_artifact_reference(&artifact.artifact_reference)
                || artifact.size_bytes == 0
                || artifact.sha256.len() != 64
                || !artifact
                    .sha256
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                || artifact.signed
                    != matches!(evidence, FirmwareResult16::SignedUpdateFirmware { .. })
        })
        || matches!(
            (evidence, evidence.reply()),
            (
                FirmwareResult16::UpdateFirmware { .. },
                Some(uob_contracts::FirmwareReply16::Status { .. })
            ) | (
                FirmwareResult16::SignedUpdateFirmware { .. },
                Some(uob_contracts::FirmwareReply16::Acknowledged)
            )
        )
    {
        return Err(conflict("invalid firmware result evidence"));
    }
    if let CommandLifecycle::ProtocolResponse { accepted, error } = &result.lifecycle
        && (*accepted != evidence.accepted()
            || error.as_ref().is_some_and(|e| {
                *accepted || e.code != CommandErrorCode::ProtocolRejected || e.detail.is_some()
            }))
    {
        return Err(conflict("firmware native reply and lifecycle disagree"));
    }
    Ok(())
}

pub(crate) fn correlate(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let command = owner(connection, result.return_route.request_id.as_str())?;
    if result.resource != command.resource
        || result.correlation_id != command.correlation_id
        || result.return_route != command.return_route()
    {
        return Err(conflict("firmware result owner changed"));
    }
    let Some(evidence) = &result.firmware_16 else {
        return Ok(());
    };
    validate_result(result)?;
    let (variant, reference) = request(&command).ok_or_else(|| conflict("not a firmware owner"))?;
    let matches = match (evidence, variant) {
        (FirmwareResult16::UpdateFirmware { .. }, FirmwareVariant16::Legacy) => true,
        (
            FirmwareResult16::SignedUpdateFirmware { request_id, .. },
            FirmwareVariant16::Signed { request_id: owner },
        ) => *request_id == owner,
        _ => false,
    } && evidence
        .artifact()
        .is_none_or(|artifact| artifact.artifact_reference == reference);
    if matches {
        Ok(())
    } else {
        Err(conflict("firmware result changed immutable request"))
    }
}

/// Native reply and sent-artifact facts are immutable once recorded.
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = &previous.firmware_16 else {
        return Ok(());
    };
    let Some(new) = &mut incoming.firmware_16 else {
        incoming.firmware_16 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_FIRMWARE_16;
        return Ok(());
    };
    let same_request = match (old, &*new) {
        (FirmwareResult16::UpdateFirmware { .. }, FirmwareResult16::UpdateFirmware { .. }) => true,
        (
            FirmwareResult16::SignedUpdateFirmware { request_id: a, .. },
            FirmwareResult16::SignedUpdateFirmware { request_id: b, .. },
        ) => a == b,
        _ => false,
    };
    if !same_request
        || (old.reply().is_some() && new.reply().is_some() && old.reply() != new.reply())
        || (old.artifact().is_some()
            && new.artifact().is_some()
            && old.artifact() != new.artifact())
    {
        return Err(conflict("firmware native reply is immutable"));
    }
    let job = new.job().clone();
    let reply = new.reply().or(old.reply());
    let artifact = new.artifact().or(old.artifact()).cloned();
    *new = match old {
        FirmwareResult16::UpdateFirmware { .. } => FirmwareResult16::UpdateFirmware {
            artifact,
            reply,
            job,
        },
        FirmwareResult16::SignedUpdateFirmware { request_id, .. } => {
            FirmwareResult16::SignedUpdateFirmware {
                request_id: *request_id,
                artifact,
                reply,
                job,
            }
        }
    };
    Ok(())
}

pub(crate) fn validate_stored(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let Some(stored) = &result.firmware_16 else {
        return Ok(());
    };
    let integrity = |detail| StorageError::new(StorageErrorCode::IntegrityFailure, detail);
    correlate(connection, result).map_err(|_| integrity("invalid stored firmware result owner"))?;
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM firmware16_jobs WHERE request_id=?1",
            [result.return_route.request_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::configuration::unavailable)?;
    let record = crate::codec_firmware16::decode(
        &payload.ok_or_else(|| integrity("stored firmware result lacks durable job"))?,
    )?;
    if *stored.job() != evidence(&record) {
        return Err(integrity("stored firmware job evidence changed"));
    }
    Ok(())
}
