//! Pinned, explicitly supported privileged commands. Protocol edition is not a capability.
use rust_ocpp::v1_6::messages::change_availability::ChangeAvailabilityRequest;
use serde::Serialize;
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, Connectivity, NativeProtocolReference, Operation, PrivilegedOcppOperation,
    ProtocolEdition, ResourceRef, StationSnapshot, ValueType,
};

const V16_SCHEMA: &str = "urn:OCPP:1.6:2019:12:ChangeAvailabilityRequest";
const V201_SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:ChangeAvailabilityRequest";

/// One resource-scoped, pinned command schema that the management API may expose.
#[derive(Clone, Debug, Serialize)]
pub struct CommandSchemaDescriptor {
    pub resource: ResourceRef,
    pub protocol: ProtocolEdition,
    pub action: &'static str,
    pub payload_schema: &'static str,
    pub fields: Vec<CommandSchemaField>,
}

/// A field of a pinned privileged request schema.
#[derive(Clone, Debug, Serialize)]
pub struct CommandSchemaField {
    pub name: &'static str,
    pub value_type: ValueType,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<&'static str>>,
}

/// Discovers only operations both pinned here and explicitly advertised by the station.
#[must_use]
pub fn command_schemas(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let Connectivity::Connected { protocol, .. } = snapshot.connectivity else {
        return Vec::new();
    };
    if !snapshot.capabilities.supports(&Operation::ProtocolAction {
        protocol,
        action: "ChangeAvailability".to_owned(),
    }) {
        return Vec::new();
    }
    let (schema, fields) = match protocol {
        ProtocolEdition::Ocpp16j => {
            if !matches!(
                snapshot.station.native_protocol_reference,
                None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
            ) {
                return Vec::new();
            }
            (
                V16_SCHEMA,
                vec![
                    CommandSchemaField {
                        name: "connectorId",
                        value_type: ValueType::UnsignedInteger,
                        required: true,
                        enum_values: None,
                    },
                    CommandSchemaField {
                        name: "type",
                        value_type: ValueType::NamedEnum,
                        required: true,
                        enum_values: Some(vec!["Operative", "Inoperative"]),
                    },
                ],
            )
        }
        ProtocolEdition::Ocpp201 => {
            if snapshot.station.native_protocol_reference.is_some() {
                return Vec::new();
            }
            (
                V201_SCHEMA,
                vec![CommandSchemaField {
                    name: "operationalStatus",
                    value_type: ValueType::NamedEnum,
                    required: true,
                    enum_values: Some(vec!["Operative", "Inoperative"]),
                }],
            )
        }
    };
    vec![CommandSchemaDescriptor {
        resource: snapshot.station.clone(),
        protocol,
        action: "ChangeAvailability",
        payload_schema: schema,
        fields,
    }]
}

/// Validates an untrusted privileged request against the exact pinned schema and scope.
/// Capability and authorization remain separate checks at admission and dispatch.
/// # Errors
/// Rejects an unlisted action or protocol and every schema, field, or scope mismatch.
pub fn validate_privileged_operation(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<(), CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, UnsupportedOperation};
    if operation.action.as_str() != "ChangeAvailability" {
        return Err(UnsupportedOperation);
    }
    if resource.resource.is_some() {
        return Err(InvalidParameters);
    }
    let payload = operation.payload.as_object().ok_or(InvalidParameters)?;
    match operation.protocol {
        ProtocolEdition::Ocpp16j => {
            if operation.payload_schema.as_str() != V16_SCHEMA
                || !matches!(
                    resource.native_protocol_reference,
                    None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
                )
                || payload.len() != 2
                || payload.get("connectorId").and_then(Value::as_u64) != Some(0)
                || !matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some("Operative" | "Inoperative")
                )
            {
                return Err(InvalidParameters);
            }
            let _: ChangeAvailabilityRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
        }
        ProtocolEdition::Ocpp201 => {
            if operation.payload_schema.as_str() != V201_SCHEMA
                || resource.native_protocol_reference.is_some()
                || payload.len() != 1
                || !matches!(
                    payload.get("operationalStatus").and_then(Value::as_str),
                    Some("Operative" | "Inoperative")
                )
            {
                return Err(InvalidParameters);
            }
            let _: rust_ocpp::v2_0_1::messages::change_availability::ChangeAvailabilityRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;
        }
    }
    Ok(())
}
