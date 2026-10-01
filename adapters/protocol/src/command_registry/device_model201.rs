use super::{CommandSchemaDescriptor, CommandSchemaField};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, sync::LazyLock};
use uob_contracts::{
    CanonicalResource, CommandErrorCode, DeviceAttributeType201, DeviceComponent201,
    DeviceComponentCriterion201, DeviceModelQuery201, DeviceReportBase201, DeviceSelector201,
    DeviceVariable201, DeviceVariableQuery201, NativeProtocolReference, PrivilegedOcppOperation,
    ProtocolEdition, ResourceRef, ValueType,
};

pub const ACTIONS: [&str; 3] = ["GetVariables", "GetBaseReport", "GetReport"];
const SCHEMAS: [&str; 8] = [
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetVariablesRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetVariablesResponse.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetBaseReportRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetBaseReportResponse.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetReportRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetReportResponse.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyReportRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/NotifyReportResponse.json"),
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
fn opaque(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.contains_key("customData") || fields.values().any(opaque),
        Value::Array(values) => values.iter().any(opaque),
        _ => false,
    }
}
pub fn contained(resource: &ResourceRef, component: &DeviceComponent201) -> bool {
    match (&resource.resource, resource.native_protocol_reference) {
        (None, None) => true,
        (
            Some(CanonicalResource::Evse {
                connector_id: canonical_connector,
                ..
            }),
            Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id,
            }),
        ) => {
            canonical_connector.is_some() == connector_id.is_some()
                && component.evse.as_ref().is_some_and(|evse| {
                    u32::try_from(evse.id).ok() == Some(evse_id)
                        && connector_id.is_none_or(|id| {
                            evse.connector_id.and_then(|v| u32::try_from(v).ok()) == Some(id)
                        })
                })
        }
        _ => false,
    }
}
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<DeviceModelQuery201, CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    let index = ACTIONS
        .iter()
        .position(|action| *action == operation.action.as_str())
        .ok_or(CommandErrorCode::UnsupportedOperation)?;
    if operation.protocol != ProtocolEdition::Ocpp201
        || operation.payload_schema.as_str()
            != format!("urn:OCPP:Cp:2:2020:3:{}Request", ACTIONS[index])
        || !valid_schema(index * 2, &operation.payload)
        || opaque(&operation.payload)
    {
        return Err(InvalidParameters);
    }
    let payload = &operation.payload;
    let query = match index {
        0 => {
            if payload["getVariableData"]
                .as_array()
                .is_none_or(|entries| entries.len() > 4096)
            {
                return Err(InvalidParameters);
            }
            let entries = Vec::<DeviceVariableQuery201>::deserialize(&payload["getVariableData"])
                .map_err(|_| InvalidParameters)?;
            let mut identities = BTreeSet::new();
            if entries.iter().any(|entry| {
                !contained(resource, &entry.component)
                    || !valid_component(&entry.component)
                    || !identities.insert(identity_key(
                        &entry.component,
                        &entry.variable,
                        entry.attribute_type.unwrap_or_default(),
                    ))
            }) {
                return Err(InvalidParameters);
            }
            DeviceModelQuery201::GetVariables { entries }
        }
        1 => {
            if resource.resource.is_some() || resource.native_protocol_reference.is_some() {
                return Err(InvalidParameters);
            }
            DeviceModelQuery201::GetBaseReport {
                request_id: i32::try_from(payload["requestId"].as_i64().ok_or(InvalidParameters)?)
                    .map_err(|_| InvalidParameters)?,
                report_base: DeviceReportBase201::deserialize(&payload["reportBase"])
                    .map_err(|_| InvalidParameters)?,
            }
        }
        _ => {
            if payload.get("componentVariable").is_some_and(|entries| {
                entries
                    .as_array()
                    .is_none_or(|entries| entries.len() > 4096)
            }) {
                return Err(InvalidParameters);
            }
            let selectors = payload
                .get("componentVariable")
                .map(Vec::<DeviceSelector201>::deserialize)
                .transpose()
                .map_err(|_| InvalidParameters)?
                .unwrap_or_default();
            let criteria = payload
                .get("componentCriteria")
                .map(Vec::<DeviceComponentCriterion201>::deserialize)
                .transpose()
                .map_err(|_| InvalidParameters)?
                .unwrap_or_default();
            if selectors.len() > 4096
                || selectors.iter().any(|selector| {
                    !valid_component(&selector.component)
                        || !contained(resource, &selector.component)
                })
                || (resource.resource.is_some() && (selectors.is_empty() || !criteria.is_empty()))
            {
                return Err(InvalidParameters);
            }
            DeviceModelQuery201::GetReport {
                request_id: i32::try_from(payload["requestId"].as_i64().ok_or(InvalidParameters)?)
                    .map_err(|_| InvalidParameters)?,
                selectors,
                criteria,
            }
        }
    };
    Ok(query)
}
pub fn valid_component(component: &DeviceComponent201) -> bool {
    component
        .evse
        .as_ref()
        .is_none_or(|evse| evse.id > 0 && evse.connector_id.is_none_or(|id| id > 0))
}
pub type IdentityKey = (
    String,
    Option<String>,
    Option<(i32, Option<i32>)>,
    String,
    Option<String>,
    u8,
);

/// Normalize once for bounded lookup; never change the preserved native fields.
pub fn identity_key(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
    attribute: DeviceAttributeType201,
) -> IdentityKey {
    let attribute = match attribute {
        DeviceAttributeType201::Actual => 0,
        DeviceAttributeType201::Target => 1,
        DeviceAttributeType201::MinSet => 2,
        DeviceAttributeType201::MaxSet => 3,
    };
    (
        caseless::default_case_fold_str(&component.name),
        component
            .instance
            .as_deref()
            .map(caseless::default_case_fold_str),
        component
            .evse
            .as_ref()
            .map(|evse| (evse.id, evse.connector_id)),
        caseless::default_case_fold_str(&variable.name),
        variable
            .instance
            .as_deref()
            .map(caseless::default_case_fold_str),
        attribute,
    )
}
pub fn descriptor(resource: ResourceRef, index: usize) -> CommandSchemaDescriptor {
    let fields = match index {
        0 => vec![
            ("getVariableData[].component.name", ValueType::Text, true),
            (
                "getVariableData[].component.instance",
                ValueType::Text,
                false,
            ),
            (
                "getVariableData[].component.evse.id",
                ValueType::SignedInteger,
                false,
            ),
            (
                "getVariableData[].component.evse.connectorId",
                ValueType::SignedInteger,
                false,
            ),
            ("getVariableData[].variable.name", ValueType::Text, true),
            (
                "getVariableData[].variable.instance",
                ValueType::Text,
                false,
            ),
            (
                "getVariableData[].attributeType",
                ValueType::NamedEnum,
                false,
            ),
        ],
        1 => vec![
            ("requestId", ValueType::SignedInteger, true),
            ("reportBase", ValueType::NamedEnum, true),
        ],
        _ => vec![
            ("requestId", ValueType::SignedInteger, true),
            ("componentVariable[].component.name", ValueType::Text, false),
            (
                "componentVariable[].component.instance",
                ValueType::Text,
                false,
            ),
            (
                "componentVariable[].component.evse.id",
                ValueType::SignedInteger,
                false,
            ),
            (
                "componentVariable[].component.evse.connectorId",
                ValueType::SignedInteger,
                false,
            ),
            ("componentVariable[].variable.name", ValueType::Text, false),
            (
                "componentVariable[].variable.instance",
                ValueType::Text,
                false,
            ),
            ("componentCriteria[]", ValueType::NamedEnum, false),
        ],
    };
    CommandSchemaDescriptor {
        resource,
        protocol: ProtocolEdition::Ocpp201,
        action: ACTIONS[index],
        payload_schema: [
            "urn:OCPP:Cp:2:2020:3:GetVariablesRequest",
            "urn:OCPP:Cp:2:2020:3:GetBaseReportRequest",
            "urn:OCPP:Cp:2:2020:3:GetReportRequest",
        ][index],
        fields: fields
            .into_iter()
            .map(|(name, value_type, required)| CommandSchemaField {
                name,
                value_type,
                required,
                enum_values: match name {
                    "getVariableData[].attributeType" => {
                        Some(vec!["Actual", "Target", "MinSet", "MaxSet"])
                    }
                    "reportBase" => Some(vec![
                        "FullInventory",
                        "ConfigurationInventory",
                        "SummaryInventory",
                    ]),
                    "componentCriteria[]" => {
                        Some(vec!["Active", "Available", "Enabled", "Problem"])
                    }
                    _ => None,
                },
            })
            .collect(),
    }
}
