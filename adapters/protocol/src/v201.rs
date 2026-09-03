//! OCPP 2.0.1 model isolation and charger-to-application mappings.

use std::fmt::Write as _;

use rust_ocpp::v2_0_1::datatypes::meter_value_type::MeterValueType;
use rust_ocpp::v2_0_1::messages::{
    boot_notification::BootNotificationRequest, heartbeat::HeartbeatRequest,
    meter_values::MeterValuesRequest, transaction_event::TransactionEventRequest,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uob_application::{
    ChargerObservation, MeasurementObservation, RegistrationObservation, TransactionEventKind,
    TransactionEventObservation,
};
use uob_contracts::{
    DataPointValue, ExactDecimal, Freshness, MeasurementContext, MeasurementLocation,
    MeasurementMetadata, MeasurementPhase, NativeProtocolReference, PointId, ProtocolActionName,
    ProtocolEdition, Quality, QualityLevel, SemanticName, TypedValue, UtcTimestamp,
};
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
        "MeterValues" => {
            let request: MeterValuesRequest = payload_as(payload)?;
            let evse_id = u32::try_from(request.evse_id)
                .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
            measurements(
                NativeProtocolReference::Ocpp201 {
                    evse_id,
                    connector_id: None,
                },
                None,
                None,
                request.meter_value,
            )?
        }
        "TransactionEvent" => {
            let request: TransactionEventRequest = payload_as(payload)?;
            transaction_event(request)?
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

fn transaction_event(request: TransactionEventRequest) -> Result<ChargerObservation, DecodeError> {
    let payload_fingerprint = serde_json::to_vec(&request)
        .map(|bytes| hex_digest(&bytes))
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    if request.seq_no < 0 {
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
    let native_resource = NativeProtocolReference::Ocpp201 {
        evse_id,
        connector_id,
    };
    let event = match format!("{:?}", request.event_type).as_str() {
        "Started" => TransactionEventKind::Started,
        "Updated" => TransactionEventKind::Updated,
        "Ended" => TransactionEventKind::Ended,
        _ => return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)),
    };
    let native_transaction_id = request.transaction_info.transaction_id;
    let sequence_number = request.seq_no.cast_unsigned();
    let meter_observation = request
        .meter_value
        .map(|values| {
            measurements(
                native_resource,
                Some(native_transaction_id.clone()),
                Some(sequence_number),
                values,
            )
        })
        .transpose()?
        .map(|observation| {
            let ChargerObservation::Measurements(observation) = observation else {
                unreachable!("measurement decoder always returns measurements")
            };
            observation
        });
    Ok(ChargerObservation::TransactionEvent(
        TransactionEventObservation {
            protocol: PROTOCOL,
            event,
            native_transaction_id,
            native_resource,
            sequence_number,
            trigger_reason: format!("{:?}", request.trigger_reason),
            charging_state: request
                .transaction_info
                .charging_state
                .map(|value| format!("{value:?}")),
            stopped_reason: request
                .transaction_info
                .stopped_reason
                .map(|value| format!("{value:?}")),
            occurred_at: timestamp(request.timestamp)?,
            measurements: meter_observation,
            payload_fingerprint,
        },
    ))
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

fn measurements(
    native_resource: NativeProtocolReference,
    native_transaction_id: Option<String>,
    sequence_number: Option<u32>,
    meter_values: Vec<MeterValueType>,
) -> Result<ChargerObservation, DecodeError> {
    if meter_values.is_empty() {
        return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
    }
    let mut values = Vec::new();
    let mut signed_values = Vec::new();
    for meter in meter_values {
        if meter.sampled_value.is_empty() {
            return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
        }
        let source_time = timestamp(meter.timestamp)?;
        for sample in meter.sampled_value {
            let original_value = sample.value.to_string();
            let multiplier = sample
                .unit_of_measure
                .as_ref()
                .and_then(|unit| unit.multiplier)
                .unwrap_or(0);
            let exact = original_value
                .parse::<ExactDecimal>()
                .and_then(|value| value.checked_scale_by_power_of_ten(multiplier))
                .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
            let measurand = sample.measurand.as_ref().map_or_else(
                || "EnergyActiveImportRegister".to_owned(),
                |value| format!("{value:?}"),
            );
            let phase = sample.phase.as_ref().map(|value| format!("{value:?}"));
            let location = sample.location.as_ref().map(|value| format!("{value:?}"));
            let context = sample.context.as_ref().map(|value| format!("{value:?}"));
            let original_unit = sample
                .unit_of_measure
                .as_ref()
                .and_then(|unit| unit.unit.clone())
                .or_else(|| Some("Wh".to_owned()));
            let point_id = PointId::new(format!(
                "ocpp201/{}/{measurand}/{}/{}",
                native_resource_key(native_resource),
                phase.as_deref().unwrap_or("all"),
                location.as_deref().unwrap_or("unspecified")
            ))
            .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
            if let Some(signed) = sample.signed_meter_value {
                signed_values
                    .push(serde_json::to_string(&signed).map_err(|_| {
                        DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
                    })?);
            }
            values.push(DataPointValue {
                point_id,
                value: Some(TypedValue::Decimal(exact)),
                source_time: Some(source_time),
                observed_at: source_time,
                quality: Quality {
                    level: QualityLevel::Good,
                    reason: None,
                },
                freshness: Freshness::Unknown,
                measurement: Some(MeasurementMetadata {
                    original_value,
                    original_unit,
                    measurand: Some(SemanticName::new(measurand).map_err(|_| {
                        DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
                    })?),
                    phase: phase
                        .map(MeasurementPhase::new)
                        .transpose()
                        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
                    context: context
                        .map(MeasurementContext::new)
                        .transpose()
                        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
                    location: location
                        .map(MeasurementLocation::new)
                        .transpose()
                        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
                    protocol_reference: Some(native_resource),
                }),
            });
        }
    }
    Ok(ChargerObservation::Measurements(MeasurementObservation {
        protocol: PROTOCOL,
        native_resource,
        native_transaction_id,
        sequence_number,
        values,
        signed_values,
    }))
}

fn native_resource_key(native_resource: NativeProtocolReference) -> String {
    match native_resource {
        NativeProtocolReference::Ocpp201 {
            evse_id,
            connector_id,
        } => connector_id.map_or_else(
            || format!("evse-{evse_id}"),
            |connector_id| format!("evse-{evse_id}/connector-{connector_id}"),
        ),
        NativeProtocolReference::Ocpp16 { connector_id } => format!("connector-{connector_id}"),
    }
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
