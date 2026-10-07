//! Destination-free OCPP 2.0.1 `GetLog` (N01). The upload location comes from the trusted
//! artifact provider at the send boundary, never from the caller.
use super::{CommandSchemaDescriptor, CommandSchemaField, firmware201::station_scope};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, GET_LOG_REFERENCE_SCHEMA_201, GetLogReference201, Operation,
    PrivilegedOcppOperation, ProtocolEdition, ResourceRef, StationSnapshot, ValueType,
};

pub const ACTION: &str = "GetLog";

/// Whether an operation belongs to this OCPP 2.0.1 module.
#[must_use]
pub fn owns(operation: &PrivilegedOcppOperation<Value>) -> bool {
    operation.protocol == ProtocolEdition::Ocpp201 && operation.action.as_str() == ACTION
}

/// Validates the exact reference schema on the station root.
///
/// # Errors
/// `UnsupportedOperation` for another edition and `InvalidParameters` for every schema,
/// field or scope mismatch.
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<GetLogReference201, CommandErrorCode> {
    if !owns(operation) {
        return Err(CommandErrorCode::UnsupportedOperation);
    }
    if operation.payload_schema.as_str() != GET_LOG_REFERENCE_SCHEMA_201 || !station_scope(resource)
    {
        return Err(CommandErrorCode::InvalidParameters);
    }
    serde_json::from_value(operation.payload.clone())
        .map_err(|_| CommandErrorCode::InvalidParameters)
}

fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values: None,
    }
}

/// Offered only on the station root and only when explicitly advertised.
#[must_use]
pub fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let advertised = snapshot.capabilities.supports(&Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp201,
        action: ACTION.to_owned(),
    });
    if !advertised || !station_scope(&snapshot.station) {
        return Vec::new();
    }
    vec![CommandSchemaDescriptor {
        resource: snapshot.station.clone(),
        protocol: ProtocolEdition::Ocpp201,
        action: ACTION,
        payload_schema: GET_LOG_REFERENCE_SCHEMA_201,
        fields: vec![
            CommandSchemaField {
                name: "logType",
                value_type: ValueType::NamedEnum,
                required: true,
                enum_values: Some(vec!["DiagnosticsLog", "SecurityLog"]),
            },
            field("requestId", ValueType::SignedInteger, true),
            field("oldestTimestamp", ValueType::Text, false),
            field("latestTimestamp", ValueType::Text, false),
            field("retries", ValueType::UnsignedInteger, false),
            field("retryInterval", ValueType::UnsignedInteger, false),
        ],
    }]
}
