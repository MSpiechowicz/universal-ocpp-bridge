use super::{conflict, validation::validate_command};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    Command, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult, ContractVersion,
    ReservationResult16, ReservationState16, ReserveNowReference16,
};

pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.reservation_16 else {
        return Ok(());
    };
    let native = match evidence {
        ReservationResult16::ReserveNow { status, .. } => status.is_some(),
        ReservationResult16::CancelReservation { status, .. } => status.is_some(),
    };
    if result.schema_version != ContractVersion::V1_RESERVATION_16
        || result.configuration.is_some()
        || !result.configuration_observations.is_empty()
        || result.configuration_201.is_some()
        || result.composite_schedule_16.is_some()
        || result.device_model_201.is_some()
        || result.charging_profile_16.is_some()
        || result.charging_profile_201.is_some()
        || result.trigger_observation.is_some()
        || result.trigger_observation_201.is_some()
        || result.local_authorization_16.is_some()
        || result.local_authorization_201.is_some()
        || result.reservation_201.is_some()
        || native != matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { .. })
    {
        return Err(conflict("invalid reservation result evidence"));
    }
    if let CommandLifecycle::ProtocolResponse { accepted, error } = &result.lifecycle
        && (*accepted != evidence.accepted()
            || error.as_ref().is_some_and(|e| {
                *accepted || e.code != CommandErrorCode::ProtocolRejected || e.detail.is_some()
            }))
    {
        return Err(conflict("reservation native status and lifecycle disagree"));
    }
    Ok(())
}
pub(crate) fn correlate(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let payload: String = connection
        .query_row(
            "SELECT payload FROM commands WHERE request_id=?1",
            [result.return_route.request_id.as_str()],
            |row| row.get(0),
        )
        .map_err(crate::configuration::unavailable)?;
    let command: Command<Value> = serde_json::from_str(&payload)
        .map_err(|_| conflict("invalid reservation command owner"))?;
    validate_command(&command)?;
    if result.resource != command.resource
        || result.correlation_id != command.correlation_id
        || result.return_route != command.return_route()
    {
        return Err(conflict("reservation result owner changed"));
    }
    let Some(evidence) = &result.reservation_16 else {
        return Ok(());
    };
    validate_result(result)?;
    let CommandOperation::Ocpp(operation) = command.operation else {
        return Err(conflict("reservation result lacks native owner"));
    };
    let valid = match evidence {
        ReservationResult16::ReserveNow {
            reservation_id,
            connector_id,
            ..
        } if operation.action.as_str() == "ReserveNow" => serde_json::from_value::<
            ReserveNowReference16,
        >(operation.payload)
        .is_ok_and(|r| r.reservation_id == *reservation_id && r.connector_id == *connector_id),
        ReservationResult16::CancelReservation { reservation_id, .. }
            if operation.action.as_str() == "CancelReservation" =>
        {
            operation.payload["reservationId"].as_i64() == Some(i64::from(*reservation_id))
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(conflict("reservation result changed immutable action"))
    }
}
pub(crate) fn validate_stored(
    connection: &Connection,
    result: &CommandResult,
) -> Result<(), StorageError> {
    let Some(evidence) = &result.reservation_16 else {
        return Ok(());
    };
    correlate(connection, result).map_err(|_| {
        StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "invalid stored reservation result owner",
        )
    })?;
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM reservations16 WHERE request_id=?1",
            [result.return_route.request_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::configuration::unavailable)?;
    let record = crate::codec_reservation16::decode(&payload.ok_or_else(|| {
        StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "stored reservation result lacks durable owner",
        )
    })?)?;
    let expected = if record.ambiguous {
        ReservationState16::Ambiguous
    } else {
        record.state
    };
    let observed_at = record
        .source_observed_at
        .map_or(record.changed_at, |at| at.max(record.changed_at));
    let reconciliation = match evidence {
        ReservationResult16::ReserveNow { reconciliation, .. }
        | ReservationResult16::CancelReservation { reconciliation, .. } => reconciliation,
    };
    if reconciliation.revision != record.revision
        || reconciliation.state != expected
        || reconciliation.observed_at != observed_at
        || reconciliation.source_time != record.source_time
    {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "stored reservation reconciliation changed",
        ));
    }
    Ok(())
}
