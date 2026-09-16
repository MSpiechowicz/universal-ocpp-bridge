//! OCPP 1.6J `DataTransfer` validation and ordered application completion.
mod outbound;
pub use outbound::{OutboundOutcome, OutboundRequest, send_data_transfer};

use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::timeout;
use uob_application::{
    ChargerObservation, OperationalStore,
    data_transfer::{Error, Observation, OpaqueData, Registry, Reply, commit_reply},
};
use uob_contracts::{StationSnapshot, UtcTimestamp};

use super::PROTOCOL;
use crate::{DecodeError, DecodeErrorKind, DecodedCall, OcppCallError, OcppErrorCode};

const PROVIDER_DEADLINE: Duration = Duration::from_secs(5);

/// Validates a strict OCPP 1.6J `DataTransfer` request payload.
///
/// The pinned schema permits empty strings, but never nulls, unknown members, or data larger than
/// the application-owned opaque-payload limit. Values are held as [`OpaqueData`] so they cannot
/// enter ordinary debug output.
///
/// # Errors
///
/// Returns a versioned payload error without retaining any supplied vendor payload.
pub fn observation(payload: Value) -> Result<Observation, DecodeError> {
    let Value::Object(mut object) = payload else {
        return Err(invalid_payload());
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "vendorId" | "messageId" | "data"))
    {
        return Err(invalid_payload());
    }
    let vendor_id = required_text(&mut object, "vendorId", 255)?;
    let message_id = optional_text(&mut object, "messageId", 50)?;
    let data = object
        .remove("data")
        .map(|value| match value {
            Value::String(text) => OpaqueData::new(text).map_err(|_| invalid_payload()),
            _ => Err(invalid_payload()),
        })
        .transpose()?;
    let request = Observation {
        vendor_id,
        message_id,
        data,
    };
    request.validate().map_err(|_| invalid_payload())?;
    Ok(request)
}

/// Decodes and completes an OCPP 1.6J `DataTransfer` CALL.
///
/// A successful response is returned only after application coordination has durably committed
/// its sanitized receipt facts. This function is for the authenticated station's ordered owner.
///
/// # Errors
///
/// Returns a sanitized CALLERROR for invalid frames, registration gating, unavailable providers,
/// provider deadline expiry, or a failed durable commit.
pub async fn data_transfer_call<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    frame: &[u8],
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    registry: &Registry,
    now: UtcTimestamp,
) -> Result<Value, OcppCallError> {
    let call = super::decode_call(frame).map_err(|error| error.call_error())?;
    complete_data_transfer(call, store, snapshot, registry, now).await
}

/// Completes one decoded `DataTransfer` CALL from the bounded session receiver.
///
/// The returned frame is a correlated OCPP CALLRESULT. Send its third element with the decoded
/// call's existing responder only after this function succeeds.
///
/// # Errors
///
/// Returns a sanitized error without acknowledging a request whose durable application commit
/// did not succeed.
pub async fn complete_data_transfer<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    call: DecodedCall,
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    registry: &Registry,
    now: UtcTimestamp,
) -> Result<Value, OcppCallError> {
    let DecodedCall {
        message_id,
        action,
        observation,
    } = call;
    if action.as_str() != "DataTransfer" {
        return Err(not_implemented());
    }
    let ChargerObservation::DataTransfer16(request) = observation else {
        return Err(not_implemented());
    };
    uob_application::registration::accepted(snapshot).map_err(|_| registration_error())?;
    let reply = timeout(PROVIDER_DEADLINE, registry.route(snapshot, &request))
        .await
        .map_err(|_| Error::ProviderUnavailable)
        .and_then(|result| result)
        .map_err(|_| provider_error())?;
    let reply = commit_reply(store, snapshot, &request, reply, now)
        .await
        .map_err(|_| persistence_error())?;
    Ok(json!([3, message_id, reply_payload(&reply)]))
}

fn required_text(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    maximum_characters: usize,
) -> Result<String, DecodeError> {
    let Some(Value::String(text)) = object.remove(key) else {
        return Err(invalid_payload());
    };
    if text.chars().count() > maximum_characters {
        return Err(invalid_payload());
    }
    Ok(text)
}

fn optional_text(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    maximum_characters: usize,
) -> Result<Option<String>, DecodeError> {
    if object.contains_key(key) {
        required_text(object, key, maximum_characters).map(Some)
    } else {
        Ok(None)
    }
}

fn reply_payload(reply: &Reply) -> Value {
    let mut payload = json!({"status": reply.status.as_str()});
    if let Some(data) = &reply.data {
        payload["data"] = Value::String(data.expose().to_owned());
    }
    payload
}

pub(super) const fn invalid_payload() -> DecodeError {
    DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
}

const fn error(code: OcppErrorCode, description: &'static str) -> OcppCallError {
    OcppCallError {
        protocol: PROTOCOL,
        code,
        description,
        field_path: None,
    }
}

const fn not_implemented() -> OcppCallError {
    error(
        OcppErrorCode::NotImplemented,
        "OCPP action is not implemented by data transfer",
    )
}

pub(super) const fn registration_error() -> OcppCallError {
    error(
        OcppErrorCode::ProtocolError,
        "Station registration state does not permit this operation",
    )
}

const fn provider_error() -> OcppCallError {
    error(
        OcppErrorCode::InternalError,
        "Data transfer provider is unavailable",
    )
}

pub(super) const fn persistence_error() -> OcppCallError {
    error(
        OcppErrorCode::InternalError,
        "Station state persistence failed",
    )
}
