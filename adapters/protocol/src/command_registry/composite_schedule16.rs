use super::{CommandSchemaDescriptor, CommandSchemaField};
use rust_ocpp::v1_6::messages::get_composite_schedule::GetCompositeScheduleRequest;
use serde::Deserialize;
use serde_json::Value;
use uob_contracts::{
    CanonicalResource, CommandErrorCode, CompositeScheduleRateUnit16, CompositeScheduleRequest16,
    NativeProtocolReference, PrivilegedOcppOperation, ProtocolEdition, ResourceRef, ValueType,
};

pub(crate) const SCHEMA: &str = "urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest";

pub(crate) fn connector(resource: &ResourceRef) -> Option<i32> {
    match (&resource.resource, resource.native_protocol_reference) {
        (None, None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })) => Some(0),
        (
            Some(CanonicalResource::Connector { .. }),
            Some(NativeProtocolReference::Ocpp16 { connector_id }),
        ) if connector_id > 0 => i32::try_from(connector_id).ok(),
        _ => None,
    }
}

pub(crate) fn descriptor(resource: ResourceRef) -> CommandSchemaDescriptor {
    CommandSchemaDescriptor {
        resource,
        protocol: ProtocolEdition::Ocpp16j,
        action: "GetCompositeSchedule",
        payload_schema: SCHEMA,
        fields: vec![
            CommandSchemaField {
                name: "connectorId",
                value_type: ValueType::UnsignedInteger,
                required: true,
                enum_values: None,
            },
            CommandSchemaField {
                name: "duration",
                value_type: ValueType::UnsignedInteger,
                required: true,
                enum_values: None,
            },
            CommandSchemaField {
                name: "chargingRateUnit",
                value_type: ValueType::NamedEnum,
                required: false,
                enum_values: Some(vec!["A", "W"]),
            },
        ],
    }
}

pub(crate) fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<CompositeScheduleRequest16, CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp16j || operation.payload_schema.as_str() != SCHEMA
    {
        return Err(InvalidParameters);
    }
    let payload = operation.payload.as_object().ok_or(InvalidParameters)?;
    if payload
        .keys()
        .any(|key| !["connectorId", "duration", "chargingRateUnit"].contains(&key.as_str()))
    {
        return Err(InvalidParameters);
    }
    let integer = |key| {
        payload
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(InvalidParameters)
    };
    let connector_id = integer("connectorId")?;
    let duration = integer("duration")?;
    if connector(resource) != Some(connector_id) || duration <= 0 {
        return Err(InvalidParameters);
    }
    let charging_rate_unit = match payload.get("chargingRateUnit") {
        None => None,
        Some(Value::String(unit)) if unit == "A" => Some(CompositeScheduleRateUnit16::A),
        Some(Value::String(unit)) if unit == "W" => Some(CompositeScheduleRateUnit16::W),
        _ => return Err(InvalidParameters),
    };
    // Also require compatibility with the pinned native request representation.
    GetCompositeScheduleRequest::deserialize(&operation.payload).map_err(|_| InvalidParameters)?;
    Ok(CompositeScheduleRequest16 {
        connector_id,
        duration,
        charging_rate_unit,
    })
}
