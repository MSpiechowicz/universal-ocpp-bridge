use super::{conflict, transitions::evidence};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use uob_application::{DiagnosticsJobMutation201, StorageError, StorageErrorCode};
use uob_contracts::{
    Command, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult, ContractVersion,
    DiagnosticsReply201, GET_LOG_REFERENCE_SCHEMA_201, GetLogReference201, LogType201,
    NativeProtocolReference, ProtocolEdition, valid_log_file_name, valid_log_reason_code_201,
};

/// Immutable native identity of a stored 2.0.1 log command.
fn request(command: &Command<Value>) -> Option<(i32, LogType201)> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    if operation.action.as_str() != "GetLog"
        || operation.payload_schema.as_str() != GET_LOG_REFERENCE_SCHEMA_201
    {
        return None;
    }
    serde_json::from_value::<GetLogReference201>(operation.payload.clone())
        .ok()
        .map(|request| (request.request_id, request.log_type))
}

pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    if operation.protocol != ProtocolEdition::Ocpp201 || operation.action.as_str() != "GetLog" {
        return Ok(());
    }
    let envelope: Command<Value> = serde_json::to_value(command)
        .and_then(serde_json::from_value)
        .map_err(|_| conflict("invalid log command"))?;
    if envelope.resource.resource.is_some()
        || !matches!(
            envelope.resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
        )
        || request(&envelope).is_none()
    {
        return Err(conflict("invalid protected log command"));
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
        serde_json::from_str(&payload).map_err(|_| conflict("invalid log command owner"))?;
    validate_command(&command)?;
    Ok(command)
}

pub(crate) fn validate_mutation(
    connection: &Connection,
    mutation: &DiagnosticsJobMutation201,
) -> Result<(), StorageError> {
    let command = owner(connection, mutation.request_id.as_str())?;
    if command.resource.bridge_id != mutation.station.bridge_id
        || command.resource.station_id != mutation.station.station_id
        || mutation.station.resource.is_some()
        || command.admitted_at != mutation.admitted_at
        || request(&command) != Some((mutation.native_request_id, mutation.log_type))
    {
        return Err(conflict("log job changed immutable request"));
    }
    Ok(())
}

fn other_evidence(result: &CommandResult) -> bool {
    result.configuration.is_some()
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
        || result.firmware_16.is_some()
        || result.firmware_201.is_some()
        || result.diagnostics_16.is_some()
}

pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.diagnostics_201 else {
        return Ok(());
    };
    let native = evidence.reply.is_some();
    if result.schema_version != ContractVersion::V1_DIAGNOSTICS_201
        || other_evidence(result)
        || native != matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { .. })
        || evidence.reply.as_ref().is_some_and(|reply| match reply {
            DiagnosticsReply201::Status {
                file_name,
                reason_code,
                ..
            } => {
                file_name
                    .as_deref()
                    .is_some_and(|name| !valid_log_file_name(name))
                    || reason_code
                        .as_deref()
                        .is_some_and(|code| !valid_log_reason_code_201(code))
            }
            DiagnosticsReply201::CallError { .. } => false,
        })
        || evidence.destination.is_some_and(|destination| {
            destination.maximum_bytes == 0 || destination.log_type != evidence.log_type
        })
        || evidence
            .job
            .upload
            .as_ref()
            .is_some_and(|upload| !crate::codec_diagnostics201::valid_upload(upload))
    {
        return Err(conflict("invalid log result evidence"));
    }
    if let CommandLifecycle::ProtocolResponse { accepted, error } = &result.lifecycle
        && (*accepted != evidence.accepted()
            || error.as_ref().is_some_and(|e| {
                *accepted || e.code != CommandErrorCode::ProtocolRejected || e.detail.is_some()
            }))
    {
        return Err(conflict("log native reply and lifecycle disagree"));
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
        return Err(conflict("log result owner changed"));
    }
    let Some(evidence) = &result.diagnostics_201 else {
        return Ok(());
    };
    validate_result(result)?;
    let (request_id, log_type) = request(&command).ok_or_else(|| conflict("not a log owner"))?;
    if evidence.request_id == request_id && evidence.log_type == log_type {
        Ok(())
    } else {
        Err(conflict("log result changed immutable request"))
    }
}

/// Native reply and offered-destination facts are immutable once recorded.
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = &previous.diagnostics_201 else {
        return Ok(());
    };
    let Some(new) = &mut incoming.diagnostics_201 else {
        incoming.diagnostics_201 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_DIAGNOSTICS_201;
        return Ok(());
    };
    if old.request_id != new.request_id
        || old.log_type != new.log_type
        || (old.reply.is_some() && new.reply.is_some() && old.reply != new.reply)
        || (old.destination.is_some()
            && new.destination.is_some()
            && old.destination != new.destination)
    {
        return Err(conflict("log native reply is immutable"));
    }
    let job = new.job.clone();
    let reply = new.reply.clone().or_else(|| old.reply.clone());
    let destination = new.destination.or(old.destination);
    *new = old.clone();
    new.job = job;
    new.reply = reply;
    new.destination = destination;
    Ok(())
}

pub(crate) fn validate_stored(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let Some(stored) = &result.diagnostics_201 else {
        return Ok(());
    };
    let integrity = |detail| StorageError::new(StorageErrorCode::IntegrityFailure, detail);
    correlate(connection, result).map_err(|_| integrity("invalid stored log result owner"))?;
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM diagnostics201_jobs WHERE request_id=?1",
            [result.return_route.request_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::configuration::unavailable)?;
    let record = crate::codec_diagnostics201::decode(
        &payload.ok_or_else(|| integrity("stored log result lacks durable job"))?,
    )?;
    if stored.job != evidence(&record) {
        return Err(integrity("stored log job evidence changed"));
    }
    Ok(())
}
