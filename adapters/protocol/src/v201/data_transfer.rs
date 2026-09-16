//! OCPP 2.0.1 `DataTransfer` validation and ordered application completion.
mod outbound;
pub use outbound::{OutboundOutcome, OutboundRequest, send_data_transfer};

use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::time::timeout;
use uob_application::{
    ChargerObservation, OperationalStore,
    data_transfer201::{Error, Observation, OpaqueData, Registry, Reply, StatusInfo, commit_reply},
};
use uob_contracts::{StationSnapshot, UtcTimestamp};

use super::PROTOCOL;
use crate::{DecodeError, DecodeErrorKind, DecodedCall, OcppCallError, OcppErrorCode};

const PROVIDER_DEADLINE: Duration = Duration::from_secs(5);

/// Validates a strict OCPP 2.0.1 `DataTransfer` request payload.
///
/// The OCPP data schema is unconstrained, so present JSON `null` is distinct from omitted data.
/// `customData` retains arbitrary extension members but must have its schema-required vendor ID.
/// Opaque values are bounded and redacted outside the selected provider and protocol adapter.
///
/// # Errors
///
/// Returns a versioned payload error without retaining invalid vendor content.
pub fn observation(payload: Value) -> Result<Observation, DecodeError> {
    let Value::Object(mut object) = payload else {
        return Err(invalid_payload());
    };
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "vendorId" | "messageId" | "data" | "customData"
        )
    }) {
        return Err(invalid_payload());
    }
    let request = Observation {
        vendor_id: required_text(&mut object, "vendorId", 255)?,
        message_id: optional_text(&mut object, "messageId", 50)?,
        data: optional_opaque(&mut object, "data")?,
        custom_data: optional_custom_data(&mut object, "customData")?,
    };
    request.validate().map_err(|_| invalid_payload())?;
    Ok(request)
}

/// Decodes and completes an OCPP 2.0.1 `DataTransfer` CALL.
///
/// A successful response is returned only after application coordination durably commits its
/// sanitized receipt facts. This function is for the authenticated station's ordered owner.
///
/// # Errors
///
/// Returns a sanitized CALLERROR for invalid frames, registration gating, unavailable providers,
/// provider deadline expiry, or failed durable commits.
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
/// The returned frame is a correlated OCPP CALLRESULT. Send it only after this function succeeds;
/// a provider timeout cancels its pending evaluation before any durable receipt is recorded.
///
/// # Errors
///
/// Returns a sanitized error without acknowledging a request whose durable commit did not succeed.
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
    let ChargerObservation::DataTransfer201(request) = observation else {
        return Err(not_implemented());
    };
    uob_application::registration::v201::accepted(snapshot).map_err(|_| registration_error())?;
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
    object: &mut Map<String, Value>,
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
    object: &mut Map<String, Value>,
    key: &str,
    maximum_characters: usize,
) -> Result<Option<String>, DecodeError> {
    if object.contains_key(key) {
        required_text(object, key, maximum_characters).map(Some)
    } else {
        Ok(None)
    }
}

fn optional_opaque(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<OpaqueData>, DecodeError> {
    object
        .remove(key)
        .map(|value| OpaqueData::new(value).map_err(|_| invalid_payload()))
        .transpose()
}

fn optional_custom_data(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<OpaqueData>, DecodeError> {
    let Some(value) = object.remove(key) else {
        return Ok(None);
    };
    let data = OpaqueData::new(value).map_err(|_| invalid_payload())?;
    let Some(custom) = data.expose().as_object() else {
        return Err(invalid_payload());
    };
    let Some(vendor_id) = custom.get("vendorId").and_then(Value::as_str) else {
        return Err(invalid_payload());
    };
    if vendor_id.chars().count() > 255 {
        return Err(invalid_payload());
    }
    Ok(Some(data))
}

pub(super) fn reply_payload(reply: &Reply) -> Value {
    let mut payload = json!({"status": reply.status.as_str()});
    if let Some(data) = &reply.data {
        payload["data"] = data.expose().clone();
    }
    if let Some(custom_data) = &reply.custom_data {
        payload["customData"] = custom_data.expose().clone();
    }
    if let Some(status_info) = &reply.status_info {
        payload["statusInfo"] = status_info_payload(status_info);
    }
    payload
}

fn status_info_payload(status_info: &StatusInfo) -> Value {
    let mut payload = json!({"reasonCode": status_info.reason_code});
    if let Some(additional_info) = &status_info.additional_info {
        payload["additionalInfo"] = Value::String(additional_info.clone());
    }
    if let Some(custom_data) = &status_info.custom_data {
        payload["customData"] = custom_data.expose().clone();
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
