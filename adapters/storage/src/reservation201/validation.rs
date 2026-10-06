use super::conflict;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use uob_application::{ReservationMutation201, ReservationMutationKind201, StorageError};
use uob_contracts::{
    CANCEL_RESERVATION_SCHEMA_201, CanonicalResource, Command, CommandOperation, CommandResult,
    ContractVersion, NativeProtocolReference, ProtocolEdition, RESERVE_NOW_REFERENCE_SCHEMA_201,
    ReservationResult201, ReserveNowReference201, ResourceRef,
};

pub(crate) fn station_scope(resource: &ResourceRef) -> bool {
    resource.resource.is_none()
        && matches!(
            resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
        )
}
fn evse_scope(resource: &ResourceRef, evse: u32) -> bool {
    matches!(
        resource.resource,
        Some(CanonicalResource::Evse {
            connector_id: None,
            ..
        })
    ) && resource.native_protocol_reference
        == Some(NativeProtocolReference::Ocpp201 {
            evse_id: evse,
            connector_id: None,
        })
}
/// OCPP 1.6 reservation commands are owned by the 1.6 validator; this covers 2.0.1 only.
pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    if !["ReserveNow", "CancelReservation"].contains(&operation.action.as_str())
        || operation.protocol != ProtocolEdition::Ocpp201
    {
        return Ok(());
    }
    let payload = serde_json::to_value(&operation.payload)
        .map_err(|_| conflict("invalid reservation payload"))?;
    let valid = if operation.action.as_str() == "ReserveNow" {
        operation.payload_schema.as_str() == RESERVE_NOW_REFERENCE_SCHEMA_201
            && serde_json::from_value::<ReserveNowReference201>(payload).is_ok_and(|r| {
                r.evse_id.map_or_else(
                    || station_scope(&command.resource),
                    |evse| evse_scope(&command.resource, evse),
                )
            })
    } else {
        operation.payload_schema.as_str() == CANCEL_RESERVATION_SCHEMA_201
            && station_scope(&command.resource)
            && payload.as_object().is_some_and(|o| o.len() == 1)
            && payload["reservationId"]
                .as_i64()
                .is_some_and(|id| i32::try_from(id).is_ok())
    };
    if valid {
        Ok(())
    } else {
        Err(conflict("invalid protected reservation command"))
    }
}
pub(crate) fn validate_mutation(
    connection: &Connection,
    mutation: &ReservationMutation201,
) -> Result<(), StorageError> {
    let payload: String = connection
        .query_row(
            "SELECT payload FROM commands WHERE request_id=?1",
            [mutation.request_id.as_str()],
            |row| row.get(0),
        )
        .map_err(crate::configuration::unavailable)?;
    let command: Command<Value> = serde_json::from_str(&payload)
        .map_err(|_| conflict("invalid reservation command owner"))?;
    validate_command(&command)?;
    if command.resource.bridge_id != mutation.station.bridge_id
        || command.resource.station_id != mutation.station.station_id
        || !station_scope(&mutation.station)
        || command.admitted_at != mutation.admitted_at
    {
        return Err(conflict("reservation mutation owner changed"));
    }
    let CommandOperation::Ocpp(operation) = command.operation else {
        return Err(conflict("reservation mutation lacks native action"));
    };
    if operation.protocol != ProtocolEdition::Ocpp201 {
        return Err(conflict("invalid reservation protocol"));
    }
    let valid = match &mutation.mutation {
        ReservationMutationKind201::Reserve(candidate)
            if operation.action.as_str() == "ReserveNow" =>
        {
            serde_json::from_value::<ReserveNowReference201>(operation.payload).is_ok_and(|r| {
                r.id == mutation.reservation_id
                    && r.evse_id == candidate.evse_id
                    && r.connector_type == candidate.connector_type
                    && r.expiry_date_time == candidate.expiry_date_time
            })
        }
        ReservationMutationKind201::Cancel if operation.action.as_str() == "CancelReservation" => {
            operation.payload["reservationId"].as_i64() == Some(i64::from(mutation.reservation_id))
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(conflict("reservation mutation changed immutable request"))
    }
}
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = &previous.reservation_201 else {
        return Ok(());
    };
    let Some(new) = &mut incoming.reservation_201 else {
        incoming.reservation_201 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_RESERVATION_201;
        return Ok(());
    };
    match (old, new) {
        (
            ReservationResult201::ReserveNow {
                reservation_id: old_id,
                evse_id: old_evse,
                status: old_status,
                ..
            },
            ReservationResult201::ReserveNow {
                reservation_id,
                evse_id,
                status,
                ..
            },
        ) if old_id == reservation_id && old_evse == evse_id => {
            if old_status.is_some() && status.is_some() && old_status != status {
                return Err(conflict("reservation native status is immutable"));
            }
            if status.is_none() {
                *status = *old_status;
            }
        }
        (
            ReservationResult201::CancelReservation {
                reservation_id: old_id,
                status: old_status,
                ..
            },
            ReservationResult201::CancelReservation {
                reservation_id,
                status,
                ..
            },
        ) if old_id == reservation_id => {
            if old_status.is_some() && status.is_some() && old_status != status {
                return Err(conflict("reservation native status is immutable"));
            }
            if status.is_none() {
                *status = *old_status;
            }
        }
        _ => return Err(conflict("reservation action identity changed")),
    }
    Ok(())
}
