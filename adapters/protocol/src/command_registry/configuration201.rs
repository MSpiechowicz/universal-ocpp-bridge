use super::{CommandSchemaDescriptor, CommandSchemaField, device_model201};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, sync::LazyLock};
use uob_contracts::{
    CONFIGURATION_ITEMS_LIMIT_201, CommandErrorCode, NativeProtocolReference, Operation,
    PrivilegedOcppOperation, ProtocolEdition, ResourceRef,
    SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201, SET_VARIABLES_REFERENCE_SCHEMA_201,
    SetNetworkProfileReference201, SetVariableReference201, SetVariablesReference201,
    StationSnapshot, ValueType, valid_configuration_reference_201,
};

pub const ACTIONS: [&str; 2] = ["SetVariables", "SetNetworkProfile"];
const SCHEMAS: [&str; 4] = [
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetVariablesRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetVariablesResponse.json"),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetNetworkProfileRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/SetNetworkProfileResponse.json"
    ),
];
pub fn valid_schema(index: usize, payload: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let schema: Value = serde_json::from_str(source).expect("pinned schema JSON");
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });
    VALIDATORS[index].is_valid(payload)
}

pub enum Request {
    Variables(SetVariablesReference201),
    Network(SetNetworkProfileReference201),
}

pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201 {
        return Err(invalid);
    }
    match operation.action.as_str() {
        "SetVariables" => {
            if operation.payload_schema.as_str() != SET_VARIABLES_REFERENCE_SCHEMA_201 {
                return Err(invalid);
            }
            if operation
                .payload
                .get("setVariableData")
                .and_then(Value::as_array)
                .is_none_or(|entries| {
                    entries.is_empty() || entries.len() > CONFIGURATION_ITEMS_LIMIT_201
                })
            {
                return Err(invalid);
            }
            let request =
                SetVariablesReference201::deserialize(&operation.payload).map_err(|_| invalid)?;
            if request.set_variable_data.is_empty()
                || request.set_variable_data.len() > CONFIGURATION_ITEMS_LIMIT_201
            {
                return Err(invalid);
            }
            let mut seen = BTreeSet::new();
            for entry in &request.set_variable_data {
                if !valid_entry(resource, entry)
                    || !seen.insert(device_model201::identity_key(
                        &entry.component,
                        &entry.variable,
                        entry.attribute_type.unwrap_or_default(),
                    ))
                {
                    return Err(invalid);
                }
            }
            Ok(Request::Variables(request))
        }
        "SetNetworkProfile" => {
            if operation.payload_schema.as_str() != SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201
                || resource.resource.is_some()
                || resource.native_protocol_reference.is_some()
            {
                return Err(invalid);
            }
            let request = SetNetworkProfileReference201::deserialize(&operation.payload)
                .map_err(|_| invalid)?;
            if !valid_configuration_reference_201(&request.profile_reference) {
                return Err(invalid);
            }
            Ok(Request::Network(request))
        }
        _ => Err(CommandErrorCode::UnsupportedOperation),
    }
}
pub fn valid_entry(resource: &ResourceRef, entry: &SetVariableReference201) -> bool {
    let bounded = |text: &str| text.chars().count() <= 50;
    valid_configuration_reference_201(&entry.value_reference)
        && device_model201::contained(resource, &entry.component)
        && device_model201::valid_component(&entry.component)
        && bounded(&entry.component.name)
        && bounded(&entry.variable.name)
        && entry.component.instance.as_deref().is_none_or(bounded)
        && entry.variable.instance.as_deref().is_none_or(bounded)
}
pub fn descriptor(resource: ResourceRef, index: usize) -> CommandSchemaDescriptor {
    let fields = if index == 0 {
        vec![
            ("setVariableData[].component.name", ValueType::Text, true),
            (
                "setVariableData[].component.instance",
                ValueType::Text,
                false,
            ),
            (
                "setVariableData[].component.evse.id",
                ValueType::SignedInteger,
                false,
            ),
            (
                "setVariableData[].component.evse.connectorId",
                ValueType::SignedInteger,
                false,
            ),
            ("setVariableData[].variable.name", ValueType::Text, true),
            (
                "setVariableData[].variable.instance",
                ValueType::Text,
                false,
            ),
            (
                "setVariableData[].attributeType",
                ValueType::NamedEnum,
                false,
            ),
            ("setVariableData[].valueReference", ValueType::Text, true),
        ]
    } else {
        vec![
            ("configurationSlot", ValueType::SignedInteger, true),
            ("profileReference", ValueType::Text, true),
        ]
    };
    CommandSchemaDescriptor {
        resource,
        protocol: ProtocolEdition::Ocpp201,
        action: ACTIONS[index],
        payload_schema: [
            SET_VARIABLES_REFERENCE_SCHEMA_201,
            SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201,
        ][index],
        fields: fields
            .into_iter()
            .map(|(name, value_type, required)| CommandSchemaField {
                name,
                value_type,
                required,
                enum_values: (name == "setVariableData[].attributeType")
                    .then(|| vec!["Actual", "Target", "MinSet", "MaxSet"]),
            })
            .collect(),
    }
}

pub(super) fn append_descriptors(
    snapshot: &StationSnapshot,
    descriptors: &mut Vec<CommandSchemaDescriptor>,
) {
    for (index, action) in ACTIONS.iter().enumerate() {
        let operation = Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: (*action).to_owned(),
        };
        if snapshot.capabilities.supports(&operation)
            && snapshot.station.native_protocol_reference.is_none()
        {
            descriptors.push(descriptor(snapshot.station.clone(), index));
        }
        if index == 0 {
            for entry in &snapshot.resources {
                if entry.capabilities.supports(&operation)
                    && entry.resource.bridge_id == snapshot.station.bridge_id
                    && entry.resource.station_id == snapshot.station.station_id
                    && matches!(
                        entry.resource.native_protocol_reference,
                        Some(NativeProtocolReference::Ocpp201 { .. })
                    )
                {
                    descriptors.push(descriptor(entry.resource.clone(), index));
                }
            }
        }
    }
}
