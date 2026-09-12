//! Registration, heartbeat and status behavior using the authoritative station snapshot.
use super::{PROTOCOL, enum_token, timestamp, validated_payload};
use crate::{OcppCallError, OcppErrorCode};
use rust_ocpp::v1_6::messages::{
    boot_notification::BootNotificationRequest, heart_beat::HeartbeatRequest,
    status_notification::StatusNotificationRequest,
};
use serde_json::{Value, json};
use uob_application::{
    OperationalStore, RegistrationObservation,
    registration::{self, ConnectorStatusObservation, RegistrationDecision, RegistrationError},
};
use uob_contracts::{StationSnapshot, UtcTimestamp};

/// Handles one call inside the authenticated station's ordered state owner.
/// The host supplies an explicit boot policy decision and nonzero heartbeat/retry interval.
/// The snapshot must be the current authoritative state for that authenticated station, restored
/// before use and shared with transaction/metering processing, never a parallel per-handler copy.
/// No CALLRESULT is returned until durable persistence succeeds.
///
/// # Errors
/// Returns a sanitized, versioned CALLERROR for invalid/unsupported input, registration gating,
/// unknown connectors, or persistence failure. The caller preserves the incoming message ID.
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

/// Completes an already decoded call from the bounded OCPP session receiver.
/// Use the returned payload (element 2) with that call's existing responder.
///
/// # Errors
/// Returns a sanitized lifecycle or storage error, with no successful response before commit.
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
        uob_application::ChargerObservation::Registration(observation)
            if observation.protocol == PROTOCOL =>
        {
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
            json!({"status": status.as_str(), "currentTime": now, "interval": interval_seconds})
        }
        uob_application::ChargerObservation::Heartbeat { protocol } if protocol == PROTOCOL => {
            registration::heartbeat(store, snapshot, now)
                .await
                .map_err(|e| lifecycle_error(&e))?;
            json!({"currentTime": now})
        }
        uob_application::ChargerObservation::ConnectorStatus(observation) => {
            registration::status(store, snapshot, &observation, now)
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

pub(super) fn boot_observation(
    payload: Value,
) -> Result<RegistrationObservation, crate::DecodeError> {
    let allowed = [
        "chargePointVendor",
        "chargePointModel",
        "chargePointSerialNumber",
        "chargeBoxSerialNumber",
        "firmwareVersion",
        "iccid",
        "imsi",
        "meterSerialNumber",
        "meterType",
    ];
    fields(&payload, &allowed)?;
    text_bounds(
        &payload,
        &[
            ("chargePointVendor", 20),
            ("chargePointModel", 20),
            ("chargePointSerialNumber", 25),
            ("chargeBoxSerialNumber", 25),
            ("firmwareVersion", 50),
            ("iccid", 20),
            ("imsi", 20),
            ("meterType", 25),
            ("meterSerialNumber", 25),
        ],
    )?;
    let request: BootNotificationRequest = super::payload_as(payload)?;
    Ok(RegistrationObservation {
        protocol: PROTOCOL,
        vendor: request.charge_point_vendor,
        model: request.charge_point_model,
        boot_reason: None,
    })
}
pub(super) fn heartbeat_payload(payload: Value) -> Result<(), crate::DecodeError> {
    fields(&payload, &[])?;
    let _: HeartbeatRequest = validated_payload(payload)?;
    Ok(())
}
fn fields(payload: &Value, allowed: &[&str]) -> Result<(), crate::DecodeError> {
    only_fields(payload, allowed)
        .map_err(|_| crate::DecodeError::new(PROTOCOL, crate::DecodeErrorKind::InvalidPayload))
}

pub(super) fn status_observation(
    payload: Value,
) -> Result<ConnectorStatusObservation, crate::DecodeError> {
    let allowed = [
        "connectorId",
        "errorCode",
        "info",
        "status",
        "timestamp",
        "vendorId",
        "vendorErrorCode",
    ];
    if only_fields(&payload, &allowed).is_err() {
        return Err(crate::DecodeError::new(
            PROTOCOL,
            crate::DecodeErrorKind::InvalidPayload,
        ));
    }
    text_bounds(
        &payload,
        &[("info", 50), ("vendorId", 255), ("vendorErrorCode", 50)],
    )?;
    let request: StatusNotificationRequest = super::payload_as(payload)?;
    Ok(ConnectorStatusObservation {
        connector_id: request.connector_id,
        status: enum_token(request.status)?,
        error_code: enum_token(request.error_code)?,
        info: request.info,
        vendor_id: request.vendor_id,
        vendor_error_code: request.vendor_error_code,
        source_time: request.timestamp.map(timestamp).transpose()?,
    })
}
fn only_fields(payload: &Value, allowed: &[&str]) -> Result<(), OcppCallError> {
    if payload.as_object().is_some_and(|object| {
        object
            .iter()
            .all(|(key, value)| allowed.contains(&key.as_str()) && !value.is_null())
    }) {
        Ok(())
    } else {
        Err(error(
            OcppErrorCode::PropertyConstraintViolation,
            "Unexpected payload field",
        ))
    }
}
pub(super) fn lifecycle_error(value: &RegistrationError) -> OcppCallError {
    match value {
        RegistrationError::Storage(_) => error(
            OcppErrorCode::InternalError,
            "Station state persistence failed",
        ),
        RegistrationError::InvalidStatus => error(
            OcppErrorCode::PropertyConstraintViolation,
            "Invalid connector status",
        ),
        RegistrationError::InvalidState | RegistrationError::NotRegistered => error(
            OcppErrorCode::ProtocolError,
            "Station registration state does not permit this operation",
        ),
    }
}
fn error(code: OcppErrorCode, description: &'static str) -> OcppCallError {
    OcppCallError {
        protocol: PROTOCOL,
        code,
        description,
        field_path: None,
    }
}

// The pinned schemas allow empty optional strings; model validator minimum lengths differ.
fn text_bounds(payload: &Value, bounds: &[(&str, usize)]) -> Result<(), crate::DecodeError> {
    for (key, max) in bounds {
        if let Some(value) = payload.get(key)
            && value.as_str().is_none_or(|s| s.chars().count() > *max)
        {
            return Err(crate::DecodeError::new(
                PROTOCOL,
                crate::DecodeErrorKind::InvalidPayload,
            ));
        }
    }
    Ok(())
}
