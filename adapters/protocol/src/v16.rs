//! OCPP 1.6J model isolation and charger-to-application mappings.

use rust_ocpp::v1_6::messages::{
    boot_notification::BootNotificationRequest, heart_beat::HeartbeatRequest,
    start_transaction::StartTransactionRequest,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uob_application::{ChargerObservation, RegistrationObservation, TransactionStartObservation};
use uob_contracts::{NativeProtocolReference, ProtocolActionName, ProtocolEdition, UtcTimestamp};
use validator::Validate;

use crate::{DecodeError, DecodeErrorKind, DecodedCall};

const PROTOCOL: ProtocolEdition = ProtocolEdition::Ocpp16j;

/// Decodes a charger-originated OCPP 1.6J CALL into an application-owned observation.
///
/// Only actions with reviewed application mappings are accepted. Other valid OCPP actions fail
/// explicitly until their feature issue supplies behavior rather than a serialization-only stub.
///
/// # Errors
///
/// Returns [`DecodeError`] when the CALL envelope is malformed, the action has no implemented
/// mapping, or the payload fails typed or field validation.
pub fn decode_call(frame: &[u8]) -> Result<DecodedCall, DecodeError> {
    let (message_id, action, payload) = parse_frame(frame)?;
    let observation = match action.as_str() {
        "BootNotification" => {
            let request: BootNotificationRequest = validated_payload(payload)?;
            ChargerObservation::Registration(RegistrationObservation {
                protocol: PROTOCOL,
                vendor: request.charge_point_vendor,
                model: request.charge_point_model,
                boot_reason: None,
            })
        }
        "Heartbeat" => {
            let _: HeartbeatRequest = validated_payload(payload)?;
            ChargerObservation::Heartbeat { protocol: PROTOCOL }
        }
        "StartTransaction" => {
            let request: StartTransactionRequest = validated_payload(payload)?;
            ChargerObservation::TransactionStarted(TransactionStartObservation {
                protocol: PROTOCOL,
                native_transaction_id: None,
                native_resource: NativeProtocolReference::Ocpp16 {
                    connector_id: request.connector_id,
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

fn validated_payload<T>(payload: Value) -> Result<T, DecodeError>
where
    T: DeserializeOwned + Validate,
{
    let value: T = serde_json::from_value(payload)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    value
        .validate()
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    Ok(value)
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
