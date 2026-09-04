//! OCPP 1.6J model isolation and charger-to-application mappings.

mod authorization;

pub use authorization::{Ocpp16AuthorizationFlow, Ocpp16AuthorizationOutcome, authorize_call};

use rust_ocpp::v1_6::messages::{
    boot_notification::BootNotificationRequest, heart_beat::HeartbeatRequest,
    meter_values::MeterValuesRequest, start_transaction::StartTransactionRequest,
};
use rust_ocpp::v1_6::types::{
    MeterValue, ReadingContext, SampledValue, UnitOfMeasure, ValueFormat,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uob_application::{
    ChargerObservation, MeasurementObservation, RegistrationObservation,
    TransactionStartObservation,
};
use uob_contracts::{
    DataPointValue, ExactDecimal, ExactDecimalError, Freshness, MeasurementContext,
    MeasurementLocation, MeasurementMetadata, MeasurementPhase, NativeProtocolReference, PointId,
    ProtocolActionName, ProtocolEdition, Quality, QualityLevel, SemanticName, TypedValue,
    UtcTimestamp,
};
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
        "MeterValues" => measurements(payload_as(payload)?)?,
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

fn measurements(request: MeterValuesRequest) -> Result<ChargerObservation, DecodeError> {
    if request.meter_value.is_empty() {
        return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
    }
    let native_resource = NativeProtocolReference::Ocpp16 {
        connector_id: request.connector_id,
    };
    let mut values = Vec::new();
    let mut signed_values = Vec::new();
    for meter in request.meter_value {
        append_meter_values(native_resource, meter, &mut values, &mut signed_values)?;
    }
    Ok(ChargerObservation::Measurements(MeasurementObservation {
        protocol: PROTOCOL,
        native_resource,
        native_transaction_id: request.transaction_id.map(|value| value.to_string()),
        sequence_number: None,
        values,
        signed_values,
    }))
}

fn append_meter_values(
    native_resource: NativeProtocolReference,
    meter: MeterValue,
    values: &mut Vec<DataPointValue>,
    signed_values: &mut Vec<String>,
) -> Result<(), DecodeError> {
    if meter.sampled_value.is_empty() {
        return Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload));
    }
    let source_time = timestamp(meter.timestamp)?;
    for sample in meter.sampled_value {
        values.push(map_sample(
            native_resource,
            source_time,
            sample,
            signed_values,
        )?);
    }
    Ok(())
}

fn map_sample(
    native_resource: NativeProtocolReference,
    source_time: UtcTimestamp,
    sample: SampledValue,
    signed_values: &mut Vec<String>,
) -> Result<DataPointValue, DecodeError> {
    let original_value = sample.value;
    let measurand = enum_token(sample.measurand.unwrap_or_default())?;
    let phase = sample.phase.map(enum_token).transpose()?;
    let context = enum_token(sample.context.unwrap_or(ReadingContext::SamplePeriodic))?;
    let location = enum_token(sample.location.unwrap_or_default())?;
    let unit = sample.unit;
    let original_unit = unit
        .clone()
        .map(enum_token)
        .transpose()?
        .or_else(|| is_energy_measurand(&measurand).then(|| "Wh".to_owned()));
    let format = sample.format.unwrap_or_default();
    let (value, quality) = match format {
        ValueFormat::Raw => raw_value(&original_value, unit.as_ref()),
        ValueFormat::SignedData => {
            signed_values.push(original_value.clone());
            (
                None,
                Quality {
                    level: QualityLevel::Uncertain,
                    reason: Some("signed_data_unverified".to_owned()),
                },
            )
        }
    };
    let point_id = PointId::new(format!(
        "ocpp16/connector-{}/{measurand}/{}/{}",
        match native_resource {
            NativeProtocolReference::Ocpp16 { connector_id } => connector_id,
            NativeProtocolReference::Ocpp201 { .. } => unreachable!("OCPP 1.6 mapping"),
        },
        phase.as_deref().unwrap_or("all"),
        location
    ))
    .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    Ok(DataPointValue {
        point_id,
        value,
        source_time: Some(source_time),
        observed_at: source_time,
        quality,
        freshness: Freshness::Unknown,
        measurement: Some(MeasurementMetadata {
            original_value,
            original_unit,
            measurand: Some(
                SemanticName::new(measurand)
                    .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
            ),
            phase: phase
                .map(MeasurementPhase::new)
                .transpose()
                .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
            context: Some(
                MeasurementContext::new(context)
                    .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
            ),
            location: Some(
                MeasurementLocation::new(location)
                    .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?,
            ),
            protocol_reference: Some(native_resource),
        }),
    })
}

fn raw_value(original: &str, unit: Option<&UnitOfMeasure>) -> (Option<TypedValue>, Quality) {
    let normalized = original
        .parse::<ExactDecimal>()
        .and_then(|value| value.checked_scale_by_power_of_ten(unit.map_or(0, unit_exponent)));
    match normalized {
        Ok(value) => (
            Some(TypedValue::Decimal(value)),
            Quality {
                level: QualityLevel::Good,
                reason: None,
            },
        ),
        Err(error) => (
            None,
            Quality {
                level: QualityLevel::Bad,
                reason: Some(
                    match error {
                        ExactDecimalError::InvalidSyntax => "invalid_decimal",
                        ExactDecimalError::Overflow => "decimal_out_of_range",
                    }
                    .to_owned(),
                ),
            },
        ),
    }
}

fn is_energy_measurand(measurand: &str) -> bool {
    measurand.starts_with("Energy.")
}

const fn unit_exponent(unit: &UnitOfMeasure) -> i32 {
    match unit {
        UnitOfMeasure::KWh
        | UnitOfMeasure::Kw
        | UnitOfMeasure::Kva
        | UnitOfMeasure::Kvar
        | UnitOfMeasure::Kvarh => 3,
        _ => 0,
    }
}

fn enum_token(value: impl serde::Serialize) -> Result<String, DecodeError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))
}

fn payload_as<T: DeserializeOwned>(payload: Value) -> Result<T, DecodeError> {
    serde_json::from_value(payload)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))
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
