//! Reference-only OCPP 1.6 firmware requests. The station location, signing certificate and
//! signature come from the trusted artifact provider at the send boundary.
use super::{CommandSchemaDescriptor, CommandSchemaField, station_scope};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, Operation, PrivilegedOcppOperation, ProtocolEdition, ResourceRef,
    SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16, SignedUpdateFirmwareReference16, StationSnapshot,
    UPDATE_FIRMWARE_REFERENCE_SCHEMA_16, UpdateFirmwareReference16, UtcTimestamp, ValueType,
};

pub const ACTIONS: [&str; 2] = ["UpdateFirmware", "SignedUpdateFirmware"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Legacy(UpdateFirmwareReference16),
    Signed(SignedUpdateFirmwareReference16),
}

impl Request {
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Legacy(_) => "UpdateFirmware",
            Self::Signed(_) => "SignedUpdateFirmware",
        }
    }
    #[must_use]
    pub fn artifact_reference(&self) -> &str {
        match self {
            Self::Legacy(request) => &request.artifact_reference,
            Self::Signed(request) => &request.artifact_reference,
        }
    }
    /// Latest native instant the station is allowed to wait for before acting.
    #[must_use]
    pub fn latest_start(&self) -> UtcTimestamp {
        match self {
            Self::Legacy(request) => request.retrieve_date,
            Self::Signed(request) => request
                .install_date_time
                .map_or(request.retrieve_date_time, |install| {
                    install.max(request.retrieve_date_time)
                }),
        }
    }
}

/// Validates the exact reference schema; both actions address only the station root.
///
/// # Errors
/// `UnsupportedOperation` for another edition and `InvalidParameters` for every schema,
/// field or scope mismatch.
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp16j {
        return Err(CommandErrorCode::UnsupportedOperation);
    }
    if !station_scope(resource) {
        return Err(invalid);
    }
    match operation.action.as_str() {
        "UpdateFirmware"
            if operation.payload_schema.as_str() == UPDATE_FIRMWARE_REFERENCE_SCHEMA_16 =>
        {
            serde_json::from_value(operation.payload.clone())
                .map(Request::Legacy)
                .map_err(|_| invalid)
        }
        "SignedUpdateFirmware"
            if operation.payload_schema.as_str() == SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16 =>
        {
            serde_json::from_value(operation.payload.clone())
                .map(Request::Signed)
                .map_err(|_| invalid)
        }
        _ => Err(invalid),
    }
}

fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values: None,
    }
}

/// Offers each action only on the station root and only when explicitly advertised.
#[must_use]
pub fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    ACTIONS
        .into_iter()
        .filter(|action| {
            station_scope(&snapshot.station)
                && snapshot.capabilities.supports(&Operation::ProtocolAction {
                    protocol: ProtocolEdition::Ocpp16j,
                    action: (*action).to_owned(),
                })
        })
        .map(|action| {
            let (payload_schema, mut fields) = if action == "UpdateFirmware" {
                (
                    UPDATE_FIRMWARE_REFERENCE_SCHEMA_16,
                    vec![field("retrieveDate", ValueType::Text, true)],
                )
            } else {
                (
                    SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16,
                    vec![
                        field("requestId", ValueType::SignedInteger, true),
                        field("retrieveDateTime", ValueType::Text, true),
                        field("installDateTime", ValueType::Text, false),
                    ],
                )
            };
            fields.insert(0, field("artifactReference", ValueType::Text, true));
            fields.push(field("retries", ValueType::UnsignedInteger, false));
            fields.push(field("retryInterval", ValueType::UnsignedInteger, false));
            CommandSchemaDescriptor {
                resource: snapshot.station.clone(),
                protocol: ProtocolEdition::Ocpp16j,
                action,
                payload_schema,
                fields,
            }
        })
        .collect()
}
