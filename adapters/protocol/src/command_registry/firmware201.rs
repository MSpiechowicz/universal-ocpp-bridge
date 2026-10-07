//! Reference-only OCPP 2.0.1 `UpdateFirmware` (L01/L02). The station location, signing
//! certificate and signature come from the trusted artifact provider at the send boundary.
use super::{CommandSchemaDescriptor, CommandSchemaField};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, NativeProtocolReference, Operation, PrivilegedOcppOperation, ProtocolEdition,
    ResourceRef, StationSnapshot, UPDATE_FIRMWARE_REFERENCE_SCHEMA_201, UpdateFirmwareReference201,
    UtcTimestamp, ValueType,
};

pub const ACTION: &str = "UpdateFirmware";

/// The request addresses the Charging Station itself, never an EVSE.
#[must_use]
pub fn station_scope(resource: &ResourceRef) -> bool {
    resource.resource.is_none()
        && matches!(
            resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
        )
}

/// Latest native instant the station is allowed to wait for before acting.
#[must_use]
pub fn latest_start(request: &UpdateFirmwareReference201) -> UtcTimestamp {
    request
        .install_date_time
        .map_or(request.retrieve_date_time, |install| {
            install.max(request.retrieve_date_time)
        })
}

/// Validates the exact reference schema on the station root.
///
/// # Errors
/// `UnsupportedOperation` for another edition and `InvalidParameters` for every schema,
/// field or scope mismatch.
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<UpdateFirmwareReference201, CommandErrorCode> {
    // `SignedUpdateFirmware` is a 1.6 Security Whitepaper message with no 2.0.1 counterpart.
    if operation.protocol != ProtocolEdition::Ocpp201 || operation.action.as_str() != ACTION {
        return Err(CommandErrorCode::UnsupportedOperation);
    }
    if operation.payload_schema.as_str() != UPDATE_FIRMWARE_REFERENCE_SCHEMA_201
        || !station_scope(resource)
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
        payload_schema: UPDATE_FIRMWARE_REFERENCE_SCHEMA_201,
        fields: vec![
            field("requestId", ValueType::SignedInteger, true),
            field("artifactReference", ValueType::Text, true),
            field("retrieveDateTime", ValueType::Text, true),
            field("installDateTime", ValueType::Text, false),
            field("retries", ValueType::UnsignedInteger, false),
            field("retryInterval", ValueType::UnsignedInteger, false),
        ],
    }]
}
