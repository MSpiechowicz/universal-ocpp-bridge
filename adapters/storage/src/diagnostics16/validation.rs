use super::{conflict, transitions::evidence};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use uob_application::{
    DiagnosticsJobMutation16, DiagnosticsVariant16, StorageError, StorageErrorCode,
};
use uob_contracts::{
    Command, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult, ContractVersion,
    DiagnosticsReply16, DiagnosticsResult16, GET_DIAGNOSTICS_REFERENCE_SCHEMA_16,
    GET_LOG_REFERENCE_SCHEMA_16, GetDiagnosticsReference16, GetLogReference16,
    NativeProtocolReference, ProtocolEdition, valid_log_file_name,
};

/// Immutable native identity of a stored diagnostics command.
fn request(command: &Command<Value>) -> Option<DiagnosticsVariant16> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    match operation.action.as_str() {
        "GetDiagnostics"
            if operation.payload_schema.as_str() == GET_DIAGNOSTICS_REFERENCE_SCHEMA_16 =>
        {
            serde_json::from_value::<GetDiagnosticsReference16>(operation.payload.clone())
                .ok()
                .map(|_| DiagnosticsVariant16::Diagnostics)
        }
        "GetLog" if operation.payload_schema.as_str() == GET_LOG_REFERENCE_SCHEMA_16 => {
            serde_json::from_value::<GetLogReference16>(operation.payload.clone())
                .ok()
                .map(|request| DiagnosticsVariant16::Log {
                    log_type: request.log_type,
                    request_id: request.request_id,
                })
        }
        _ => None,
    }
}

pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    // OCPP 2.0.1 `GetLog` belongs to its own edition; `GetDiagnostics` exists only in 1.6.
    let protected = match operation.action.as_str() {
        "GetDiagnostics" => true,
        "GetLog" => operation.protocol == ProtocolEdition::Ocpp16j,
        _ => false,
    };
    if !protected {
        return Ok(());
    }
    let envelope: Command<Value> = serde_json::to_value(command)
        .and_then(serde_json::from_value)
        .map_err(|_| conflict("invalid diagnostics command"))?;
    if operation.protocol != ProtocolEdition::Ocpp16j
        || envelope.resource.resource.is_some()
        || !matches!(
            envelope.resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
        )
        || request(&envelope).is_none()
    {
        return Err(conflict("invalid protected diagnostics command"));
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
    let command: Command<Value> = serde_json::from_str(&payload)
        .map_err(|_| conflict("invalid diagnostics command owner"))?;
    validate_command(&command)?;
    Ok(command)
}

pub(crate) fn validate_mutation(
    connection: &Connection,
    mutation: &DiagnosticsJobMutation16,
) -> Result<(), StorageError> {
    let command = owner(connection, mutation.request_id.as_str())?;
    if command.resource.bridge_id != mutation.station.bridge_id
        || command.resource.station_id != mutation.station.station_id
        || mutation.station.resource.is_some()
        || command.admitted_at != mutation.admitted_at
        || request(&command) != Some(mutation.variant)
    {
        return Err(conflict("diagnostics job changed immutable request"));
    }
    Ok(())
}

fn reply_matches(evidence: &DiagnosticsResult16) -> bool {
    let file_name = match (evidence, evidence.reply()) {
        (_, None | Some(DiagnosticsReply16::CallError { .. })) => None,
        (
            DiagnosticsResult16::GetDiagnostics { .. },
            Some(DiagnosticsReply16::Diagnostics { file_name }),
        )
        | (DiagnosticsResult16::GetLog { .. }, Some(DiagnosticsReply16::Log { file_name, .. })) => {
            file_name.as_deref()
        }
        _ => return false,
    };
    file_name.is_none_or(valid_log_file_name)
}

pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.diagnostics_16 else {
        return Ok(());
    };
    let native = evidence.reply().is_some();
    if result.schema_version != ContractVersion::V1_DIAGNOSTICS_16
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
        || result.firmware_16.is_some()
        || result.firmware_201.is_some()
        || result.diagnostics_201.is_some()
        || native != matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { .. })
        || !reply_matches(evidence)
        || evidence.destination().is_some_and(|destination| {
            destination.maximum_bytes == 0
                || match evidence {
                    DiagnosticsResult16::GetDiagnostics { .. } => {
                        destination.log_type != uob_contracts::LogType16::DiagnosticsLog
                    }
                    DiagnosticsResult16::GetLog { log_type, .. } => {
                        destination.log_type != *log_type
                    }
                }
        })
        || evidence.job().upload.as_ref().is_some_and(|upload| {
            upload.sha256.len() != 64
                || !upload
                    .sha256
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
    {
        return Err(conflict("invalid diagnostics result evidence"));
    }
    if let CommandLifecycle::ProtocolResponse { accepted, error } = &result.lifecycle
        && (*accepted != evidence.accepted()
            || error.as_ref().is_some_and(|e| {
                *accepted || e.code != CommandErrorCode::ProtocolRejected || e.detail.is_some()
            }))
    {
        return Err(conflict("diagnostics native reply and lifecycle disagree"));
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
        return Err(conflict("diagnostics result owner changed"));
    }
    let Some(evidence) = &result.diagnostics_16 else {
        return Ok(());
    };
    validate_result(result)?;
    let variant = request(&command).ok_or_else(|| conflict("not a diagnostics owner"))?;
    let matches = match (evidence, variant) {
        (DiagnosticsResult16::GetDiagnostics { .. }, DiagnosticsVariant16::Diagnostics) => true,
        (
            DiagnosticsResult16::GetLog {
                log_type,
                request_id,
                ..
            },
            DiagnosticsVariant16::Log {
                log_type: owner_type,
                request_id: owner,
            },
        ) => *request_id == owner && *log_type == owner_type,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(conflict("diagnostics result changed immutable request"))
    }
}

/// Native reply and offered-destination facts are immutable once recorded.
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = &previous.diagnostics_16 else {
        return Ok(());
    };
    let Some(new) = &mut incoming.diagnostics_16 else {
        incoming.diagnostics_16 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_DIAGNOSTICS_16;
        return Ok(());
    };
    let same_request = match (old, &*new) {
        (
            DiagnosticsResult16::GetDiagnostics { .. },
            DiagnosticsResult16::GetDiagnostics { .. },
        ) => true,
        (
            DiagnosticsResult16::GetLog {
                log_type: a_type,
                request_id: a,
                ..
            },
            DiagnosticsResult16::GetLog {
                log_type: b_type,
                request_id: b,
                ..
            },
        ) => a == b && a_type == b_type,
        _ => false,
    };
    if !same_request
        || (old.reply().is_some() && new.reply().is_some() && old.reply() != new.reply())
        || (old.destination().is_some()
            && new.destination().is_some()
            && old.destination() != new.destination())
    {
        return Err(conflict("diagnostics native reply is immutable"));
    }
    let job = new.job().clone();
    let reply = new.reply().or(old.reply()).cloned();
    let destination = new.destination().or(old.destination()).copied();
    *new = match old {
        DiagnosticsResult16::GetDiagnostics { .. } => DiagnosticsResult16::GetDiagnostics {
            destination,
            reply,
            job,
        },
        DiagnosticsResult16::GetLog {
            log_type,
            request_id,
            ..
        } => DiagnosticsResult16::GetLog {
            log_type: *log_type,
            request_id: *request_id,
            destination,
            reply,
            job,
        },
    };
    Ok(())
}

pub(crate) fn validate_stored(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let Some(stored) = &result.diagnostics_16 else {
        return Ok(());
    };
    let integrity = |detail| StorageError::new(StorageErrorCode::IntegrityFailure, detail);
    correlate(connection, result)
        .map_err(|_| integrity("invalid stored diagnostics result owner"))?;
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM diagnostics16_jobs WHERE request_id=?1",
            [result.return_route.request_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::configuration::unavailable)?;
    let record = crate::codec_diagnostics16::decode(
        &payload.ok_or_else(|| integrity("stored diagnostics result lacks durable job"))?,
    )?;
    if *stored.job() != evidence(&record) {
        return Err(integrity("stored diagnostics job evidence changed"));
    }
    Ok(())
}
