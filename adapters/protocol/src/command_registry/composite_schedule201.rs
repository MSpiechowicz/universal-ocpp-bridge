//! Pinned OCPP 2.0.1 `GetCompositeSchedule` (K08) request scope and schema validation.
use super::{CommandSchemaDescriptor, CommandSchemaField, charging_profile201::evse};
use rust_ocpp::v2_0_1::messages::get_composite_schedule::GetCompositeScheduleRequest;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use uob_contracts::{
    ChargingScheduleRateUnit201, CommandErrorCode, CompositeScheduleRequest201, Operation,
    PrivilegedOcppOperation, ProtocolEdition, ResourceRef, StationSnapshot, ValueType,
};

pub(crate) const ACTION: &str = "GetCompositeSchedule";
pub(crate) const SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:GetCompositeScheduleRequest";
pub(crate) const REQUEST: usize = 0;
pub(crate) const RESPONSE: usize = 1;
const SCHEMAS: [&str; 2] = [
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetCompositeScheduleRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetCompositeScheduleResponse.json"
    ),
];

pub(crate) fn valid_schema(index: usize, value: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let schema: Value = serde_json::from_str(source).expect("pinned native schema");
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });
    VALIDATORS[index].is_valid(value)
}

/// Station scope and each advertised positive EVSE, never a connector.
pub(crate) fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let required = Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp201,
        action: ACTION.to_owned(),
    };
    std::iter::once((&snapshot.station, &snapshot.capabilities))
        .chain(
            snapshot
                .resources
                .iter()
                .map(|entry| (&entry.resource, &entry.capabilities)),
        )
        .filter(|(resource, capabilities)| {
            resource.bridge_id == snapshot.station.bridge_id
                && resource.station_id == snapshot.station.station_id
                && evse(resource).is_some()
                && capabilities.supports(&required)
        })
        .map(|(resource, _)| CommandSchemaDescriptor {
            resource: resource.clone(),
            protocol: ProtocolEdition::Ocpp201,
            action: ACTION,
            payload_schema: SCHEMA,
            fields: vec![
                CommandSchemaField {
                    name: "evseId",
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
        })
        .collect()
}

/// Station scope requires `evseId: 0` (grid connection); EVSE scope its exact native ID.
/// # Errors
/// Rejects wrong edition/schema, `customData`, out-of-scope EVSEs and non-positive durations.
pub(crate) fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<CompositeScheduleRequest201, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201
        || operation.action.as_str() != ACTION
        || operation.payload_schema.as_str() != SCHEMA
        || operation.payload.get("customData").is_some()
        || !valid_schema(REQUEST, &operation.payload)
    {
        return Err(invalid);
    }
    let payload = &operation.payload;
    let integer = |key: &str| {
        payload[key]
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(invalid)
    };
    let evse_id = integer("evseId")?;
    let duration = integer("duration")?;
    if evse(resource) != Some(evse_id) || duration <= 0 {
        return Err(invalid);
    }
    let charging_rate_unit = payload
        .get("chargingRateUnit")
        .map(ChargingScheduleRateUnit201::deserialize)
        .transpose()
        .map_err(|_| invalid)?;
    // Also require compatibility with the pinned native request representation.
    GetCompositeScheduleRequest::deserialize(payload).map_err(|_| invalid)?;
    Ok(CompositeScheduleRequest201 {
        evse_id,
        duration,
        charging_rate_unit,
    })
}
