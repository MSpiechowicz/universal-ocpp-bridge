//! Explicit field validation on top of the pinned model (which ignores unknown fields).
use super::{PROTOCOL, append_meter_values, enum_token, timestamp, validated_payload};
use crate::{DecodeError, DecodeErrorKind};
use rust_ocpp::v1_6::messages::{
    start_transaction::StartTransactionRequest, stop_transaction::StopTransactionRequest,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use uob_application::{
    TransactionStartObservation,
    charging_identity::{ChargingTokenKind, PresentedChargingIdentity},
    transaction16::StopObservation,
};
use uob_contracts::NativeProtocolReference;

pub(super) fn start(payload: Value) -> Result<TransactionStartObservation, DecodeError> {
    fields(
        &payload,
        &[
            "connectorId",
            "idTag",
            "meterStart",
            "reservationId",
            "timestamp",
        ],
    )?;
    let request: StartTransactionRequest = validated_payload(payload)?;
    if request.connector_id == 0 || request.connector_id > i32::MAX.cast_unsigned() {
        return Err(invalid());
    }
    let payload_fingerprint = fingerprint(&request)?;
    Ok(TransactionStartObservation {
        protocol: PROTOCOL,
        native_transaction_id: None,
        native_resource: NativeProtocolReference::Ocpp16 {
            connector_id: request.connector_id,
        },
        occurred_at: timestamp(request.timestamp)?,
        meter_start: request.meter_start,
        reservation_id: request.reservation_id,
        fingerprint: payload_fingerprint,
        identity: PresentedChargingIdentity {
            token: request.id_tag,
            kind: ChargingTokenKind::Local,
            additional: Vec::new(),
            certificate: None,
            certificate_hashes: Vec::new(),
        },
    })
}

pub(super) fn stop(payload: Value) -> Result<StopObservation, DecodeError> {
    fields(
        &payload,
        &[
            "transactionId",
            "idTag",
            "meterStop",
            "reason",
            "timestamp",
            "transactionData",
        ],
    )?;
    if let Some(meters) = payload.get("transactionData") {
        let meters = meters.as_array().ok_or_else(invalid)?;
        let mut count = 0;
        for meter in meters {
            fields(meter, &["timestamp", "sampledValue"])?;
            let samples = meter
                .get("sampledValue")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            if samples.is_empty() {
                return Err(invalid());
            }
            count += samples.len();
            if count > 256 {
                return Err(invalid());
            }
            for sample in samples {
                fields(
                    sample,
                    &[
                        "value",
                        "context",
                        "format",
                        "measurand",
                        "phase",
                        "location",
                        "unit",
                    ],
                )?;
            }
        }
    }
    let request: StopTransactionRequest = validated_payload(payload)?;
    if request.transaction_id <= 0 {
        return Err(invalid());
    }
    let payload_fingerprint = fingerprint(&request)?;
    let mut values = Vec::new();
    let mut signed_values = Vec::new();
    for meter in request.transaction_data.unwrap_or_default() {
        // Resolve connector zero to the persisted transaction's connector in the application.
        append_meter_values(
            NativeProtocolReference::Ocpp16 { connector_id: 0 },
            meter,
            &mut values,
            &mut signed_values,
        )?;
    }
    Ok(StopObservation {
        transaction_id: request.transaction_id,
        occurred_at: timestamp(request.timestamp)?,
        meter_stop: request.meter_stop,
        reason: request.reason.map(enum_token).transpose()?,
        identity_fingerprint: request.id_tag.as_ref().map(fingerprint).transpose()?,
        fingerprint: payload_fingerprint,
        values,
        signed_values,
    })
}
fn fields(payload: &Value, allowed: &[&str]) -> Result<(), DecodeError> {
    if payload.as_object().is_none_or(|o| {
        o.iter()
            .any(|(k, v)| !allowed.contains(&k.as_str()) || v.is_null())
    }) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn invalid() -> DecodeError {
    DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
}
fn fingerprint(value: &impl serde::Serialize) -> Result<String, DecodeError> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            write!(s, "{b:02x}").expect("String write");
            s
        }))
}
