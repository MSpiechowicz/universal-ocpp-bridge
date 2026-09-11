//! OCPP 2.0.1 registration behavior behind the existing ordered station owner.
use super::{PROTOCOL, payload_as, timestamp};
use crate::{DecodeError, DecodeErrorKind, OcppCallError, OcppErrorCode};
use rust_ocpp::v2_0_1::messages::{
    boot_notification::BootNotificationRequest, status_notification::StatusNotificationRequest,
};
use serde_json::{Value, json};
use uob_application::{
    ChargerObservation, OperationalStore, RegistrationObservation,
    registration::{self, RegistrationDecision, RegistrationError, v201::StatusObservation},
};
use uob_contracts::{StationSnapshot, UtcTimestamp};

/// Processes a wire call for the authenticated station's authoritative, restored snapshot.
/// The host serializes this with every other station mutation and supplies boot policy.
/// # Errors
/// Invalid/unsupported requests and persistence failures return sanitized CALLERRORs.
pub async fn registration_call<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    frame: &[u8],
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    decision: RegistrationDecision,
    interval_seconds: u32,
    now: UtcTimestamp,
) -> Result<Value, OcppCallError> {
    let call = super::decode_call(frame).map_err(|e| e.call_error())?;
    complete_registration(call, store, snapshot, decision, interval_seconds, now).await
}

/// Completes a decoded call; send element 2 through its existing bounded responder.
/// No successful response is produced before the state commit succeeds.
/// # Errors
/// Rejects wrong protocol, registration gating, invalid resources and storage failures.
pub async fn complete_registration<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    call: crate::DecodedCall,
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    decision: RegistrationDecision,
    interval_seconds: u32,
    now: UtcTimestamp,
) -> Result<Value, OcppCallError> {
    let response = match call.observation {
        ChargerObservation::Registration(observation) if observation.protocol == PROTOCOL => {
            let status = registration::register(
                store,
                snapshot,
                &observation,
                decision,
                interval_seconds,
                now,
            )
            .await
            .map_err(|e| lifecycle_error(&e))?;
            json!({"status":status.as_str(),"currentTime":now,"interval":interval_seconds})
        }
        ChargerObservation::Heartbeat { protocol } if protocol == PROTOCOL => {
            registration::v201::heartbeat(store, snapshot, now)
                .await
                .map_err(|e| lifecycle_error(&e))?;
            json!({"currentTime":now})
        }
        ChargerObservation::EvseConnectorStatus(observation) => {
            registration::v201::status(store, snapshot, &observation, now)
                .await
                .map_err(|e| lifecycle_error(&e))?;
            json!({})
        }
        _ => {
            return Err(error(
                OcppErrorCode::NotImplemented,
                "OCPP action is not implemented by registration",
            ));
        }
    };
    Ok(json!([3, call.message_id, response]))
}

pub(super) fn boot_observation(payload: Value) -> Result<RegistrationObservation, DecodeError> {
    fields(&payload, &["reason", "chargingStation", "customData"])?;
    let station = payload.get("chargingStation").ok_or_else(invalid)?;
    fields(
        station,
        &[
            "model",
            "vendorName",
            "serialNumber",
            "firmwareVersion",
            "modem",
            "customData",
        ],
    )?;
    text_bounds(
        station,
        &[
            ("model", 20),
            ("vendorName", 50),
            ("serialNumber", 25),
            ("firmwareVersion", 50),
        ],
    )?;
    if let Some(modem) = station.get("modem") {
        fields(modem, &["iccid", "imsi", "customData"])?;
        text_bounds(modem, &[("iccid", 20), ("imsi", 20)])?;
    }
    let request: BootNotificationRequest = payload_as(payload)?;
    Ok(RegistrationObservation {
        protocol: PROTOCOL,
        vendor: request.charging_station.vendor_name,
        model: request.charging_station.model,
        boot_reason: Some(token(request.reason)?),
    })
}
pub(super) fn heartbeat_payload(payload: &Value) -> Result<(), DecodeError> {
    fields(payload, &["customData"])
}
pub(super) fn status_observation(payload: Value) -> Result<StatusObservation, DecodeError> {
    fields(
        &payload,
        &[
            "timestamp",
            "connectorStatus",
            "evseId",
            "connectorId",
            "customData",
        ],
    )?;
    let request: StatusNotificationRequest = payload_as(payload)?;
    let evse_id = u32::try_from(request.evse_id).map_err(|_| invalid())?;
    let connector_id = u32::try_from(request.connector_id).map_err(|_| invalid())?;
    if evse_id == 0 || connector_id == 0 {
        return Err(invalid());
    }
    Ok(StatusObservation {
        evse_id,
        connector_id,
        status: token(request.connector_status)?,
        source_time: timestamp(request.timestamp)?,
    })
}
fn fields(payload: &Value, allowed: &[&str]) -> Result<(), DecodeError> {
    let object = payload.as_object().ok_or_else(invalid)?;
    if object
        .iter()
        .any(|(key, value)| !allowed.contains(&key.as_str()) || value.is_null())
    {
        return Err(invalid());
    }
    // Extension contents remain inert and are never interpreted as core status/error fields.
    if let Some(custom) = object.get("customData") {
        let custom = custom.as_object().ok_or_else(invalid)?;
        if custom
            .get("vendorId")
            .and_then(Value::as_str)
            .is_none_or(|v| v.chars().count() > 255)
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn text_bounds(payload: &Value, bounds: &[(&str, usize)]) -> Result<(), DecodeError> {
    for (key, max) in bounds {
        if payload
            .get(key)
            .is_some_and(|v| v.as_str().is_none_or(|s| s.chars().count() > *max))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn token(value: impl serde::Serialize) -> Result<String, DecodeError> {
    serde_json::to_value(value)
        .map_err(|_| invalid())?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(invalid)
}
fn invalid() -> DecodeError {
    DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
}
fn error(code: OcppErrorCode, description: &'static str) -> OcppCallError {
    OcppCallError {
        protocol: PROTOCOL,
        code,
        description,
        field_path: None,
    }
}
fn lifecycle_error(value: &RegistrationError) -> OcppCallError {
    match value {
        RegistrationError::Storage(_) => error(
            OcppErrorCode::InternalError,
            "Station state persistence failed",
        ),
        RegistrationError::InvalidStatus => error(
            OcppErrorCode::PropertyConstraintViolation,
            "Invalid EVSE connector status",
        ),
        RegistrationError::InvalidState | RegistrationError::NotRegistered => error(
            OcppErrorCode::ProtocolError,
            "Station registration state does not permit this operation",
        ),
    }
}
