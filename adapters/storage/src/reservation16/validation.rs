use super::conflict;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use uob_application::{ReservationMutation16, ReservationMutationKind16, StorageError};
use uob_contracts::{
    Command, CommandOperation, CommandResult, ContractVersion, NativeProtocolReference,
    ProtocolEdition, RESERVE_NOW_REFERENCE_SCHEMA_16, ReservationResult16, ReserveNowReference16,
};

pub(crate) fn validate_command<P: Serialize>(command: &Command<P>) -> Result<(), StorageError> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(());
    };
    if !["ReserveNow", "CancelReservation"].contains(&operation.action.as_str()) {
        return Ok(());
    }
    // OCPP 2.0.1 reservations are validated by their own edition-specific owner.
    if operation.protocol == ProtocolEdition::Ocpp201 {
        return Ok(());
    }
    if operation.protocol != ProtocolEdition::Ocpp16j {
        return Err(conflict("invalid reservation protocol"));
    }
    let payload = serde_json::to_value(&operation.payload)
        .map_err(|_| conflict("invalid reservation payload"))?;
    let valid = if operation.action.as_str() == "ReserveNow" {
        operation.payload_schema.as_str() == RESERVE_NOW_REFERENCE_SCHEMA_16
            && serde_json::from_value::<ReserveNowReference16>(payload).is_ok_and(|r| {
                if r.connector_id == 0 {
                    command.resource.resource.is_none()
                        && matches!(
                            command.resource.native_protocol_reference,
                            None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
                        )
                } else {
                    matches!(
                        command.resource.resource,
                        Some(uob_contracts::CanonicalResource::Connector { .. })
                    ) && command.resource.native_protocol_reference
                        == Some(NativeProtocolReference::Ocpp16 {
                            connector_id: r.connector_id,
                        })
                }
            })
    } else {
        operation.payload_schema.as_str() == "urn:OCPP:1.6:2019:12:CancelReservationRequest"
            && command.resource.resource.is_none()
            && matches!(
                command.resource.native_protocol_reference,
                None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
            )
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
    mutation: &ReservationMutation16,
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
        || mutation.station.resource.is_some()
        || command.admitted_at != mutation.admitted_at
    {
        return Err(conflict("reservation mutation owner changed"));
    }
    let CommandOperation::Ocpp(operation) = command.operation else {
        return Err(conflict("reservation mutation lacks native action"));
    };
    let valid = match &mutation.mutation {
        ReservationMutationKind16::Reserve(candidate)
            if operation.action.as_str() == "ReserveNow" =>
        {
            serde_json::from_value::<ReserveNowReference16>(operation.payload).is_ok_and(|r| {
                r.reservation_id == mutation.reservation_id
                    && r.connector_id == candidate.connector_id
                    && r.expiry_date == candidate.expiry_date
            })
        }
        ReservationMutationKind16::Cancel if operation.action.as_str() == "CancelReservation" => {
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
    let Some(old) = &previous.reservation_16 else {
        return Ok(());
    };
    let Some(new) = &mut incoming.reservation_16 else {
        incoming.reservation_16 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_RESERVATION_16;
        return Ok(());
    };
    match (old, new) {
        (
            ReservationResult16::ReserveNow {
                reservation_id: old_id,
                connector_id: old_connector,
                status: old_status,
                ..
            },
            ReservationResult16::ReserveNow {
                reservation_id,
                connector_id,
                status,
                ..
            },
        ) if old_id == reservation_id && old_connector == connector_id => {
            if old_status.is_some() && status.is_some() && old_status != status {
                return Err(conflict("reservation native status is immutable"));
            }
            if status.is_none() {
                *status = *old_status;
            }
        }
        (
            ReservationResult16::CancelReservation {
                reservation_id: old_id,
                status: old_status,
                ..
            },
            ReservationResult16::CancelReservation {
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
