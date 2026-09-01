//! OCPP 2.0.1 model isolation and charger-to-application mappings.

use rust_ocpp::v2_0_1::messages::{
    boot_notification::BootNotificationRequest, heartbeat::HeartbeatRequest,
    transaction_event::TransactionEventRequest,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uob_application::{ChargerObservation, RegistrationObservation, TransactionStartObservation};
use uob_contracts::{NativeProtocolReference, ProtocolActionName, ProtocolEdition, UtcTimestamp};
use validator::Validate;

use crate::{DecodeError, DecodeErrorKind, DecodedCall};

const PROTOCOL: ProtocolEdition = ProtocolEdition::Ocpp201;

/// Decodes a charger-originated OCPP 2.0.1 CALL into an application-owned observation.
///
/// # Errors
///
/// Returns [`DecodeError`] when the CALL envelope is malformed, the action has no implemented
/// mapping, or the payload fails typed or field validation.
pub fn decode_call(frame: &[u8]) -> Result<DecodedCall, DecodeError> {
    let (message_id, action, payload) = parse_frame(frame)?;
    let observation = match action.as_str() {
        "BootNotification" => {
            let request: BootNotificationRequest = payload_as(payload)?;
            if request.charging_station.vendor_name.trim().is_empty()
                || request.charging_station.model.trim().is_empty()
                || request.charging_station.validate().is_err()
            {
                return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
            }
            ChargerObservation::Registration(RegistrationObservation {
                protocol: PROTOCOL,
                vendor: request.charging_station.vendor_name,
                model: request.charging_station.model,
                boot_reason: Some(format!("{:?}", request.reason)),
            })
        }
        "Heartbeat" => {
            let _: HeartbeatRequest = payload_as(payload)?;
            ChargerObservation::Heartbeat { protocol: PROTOCOL }
        }
        "TransactionEvent" => {
            let request: TransactionEventRequest = payload_as(payload)?;
            if request.seq_no < 0 || format!("{:?}", request.event_type) != "Started" {
                return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
            }
            let evse = request
                .evse
                .filter(|evse| evse.id > 0 && evse.connector_id.is_none_or(|id| id > 0))
                .ok_or_else(|| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
            let evse_id = u32::try_from(evse.id)
                .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
            let connector_id = evse
                .connector_id
                .map(u32::try_from)
                .transpose()
                .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
            if request.transaction_info.transaction_id.trim().is_empty() {
                return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
            }
            ChargerObservation::TransactionStarted(TransactionStartObservation {
                protocol: PROTOCOL,
                native_transaction_id: Some(request.transaction_info.transaction_id),
                native_resource: NativeProtocolReference::Ocpp201 {
                    evse_id,
                    connector_id,
                },
                occurred_at: timestamp(request.timestamp)?,
            })
        }
        _ => {
            return Err(DecodeError::new(
                PROTOCOL,
                DecodeErrorKind::UnsupportedAction,
            ));
        }
    };

    let action = ProtocolActionName::new(action)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::MalformedFrame))?;
    Ok(DecodedCall {
        message_id,
        action,
        observation,
    })
}

fn payload_as<T: DeserializeOwned>(payload: Value) -> Result<T, DecodeError> {
    serde_json::from_value(payload)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))
}

fn parse_frame(frame: &[u8]) -> Result<(String, String, Value), DecodeError> {
    let value: Value = serde_json::from_slice(frame)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::MalformedFrame))?;
    let items = value
        .as_array()
        .filter(|items| items.len() == 4 && items[0].as_u64() == Some(2))
        .ok_or_else(|| DecodeError::new(PROTOCOL, DecodeErrorKind::MalformedFrame))?;
    let message_id = items[1]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| DecodeError::new(PROTOCOL, DecodeErrorKind::MalformedFrame))?
        .to_owned();
    let action = items[2]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| DecodeError::new(PROTOCOL, DecodeErrorKind::MalformedFrame))?
        .to_owned();
    if !items[3].is_object() {
        return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
    }
    Ok((message_id, action, items[3].clone()))
}

fn timestamp(value: impl serde::Serialize) -> Result<UtcTimestamp, DecodeError> {
    serde_json::from_value(
        serde_json::to_value(value)
            .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
    )
    .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))
}
