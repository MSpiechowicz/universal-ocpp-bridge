//! Destination-free OCPP 1.6 diagnostics and Security Whitepaper log requests. The upload
//! location comes from the trusted artifact provider at the send boundary.
use super::{CommandSchemaDescriptor, CommandSchemaField, station_scope};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, GET_DIAGNOSTICS_REFERENCE_SCHEMA_16, GET_LOG_REFERENCE_SCHEMA_16,
    GetDiagnosticsReference16, GetLogReference16, LogType16, Operation, PrivilegedOcppOperation,
    ProtocolEdition, ResourceRef, StationSnapshot, ValueType,
};

pub const ACTIONS: [&str; 2] = ["GetDiagnostics", "GetLog"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Diagnostics(GetDiagnosticsReference16),
    Log(GetLogReference16),
}

impl Request {
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Diagnostics(_) => "GetDiagnostics",
            Self::Log(_) => "GetLog",
        }
    }
    #[must_use]
    pub const fn log_type(&self) -> LogType16 {
        match self {
            Self::Diagnostics(_) => LogType16::DiagnosticsLog,
            Self::Log(request) => request.log_type,
        }
    }
}

/// Whether an operation belongs to this OCPP 1.6 module; 2.0.1 `GetLog` belongs elsewhere.
#[must_use]
pub fn owns(operation: &PrivilegedOcppOperation<Value>) -> bool {
    match operation.action.as_str() {
        "GetDiagnostics" => true,
        "GetLog" => operation.protocol == ProtocolEdition::Ocpp16j,
        _ => false,
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
        "GetDiagnostics"
            if operation.payload_schema.as_str() == GET_DIAGNOSTICS_REFERENCE_SCHEMA_16 =>
        {
            serde_json::from_value(operation.payload.clone())
                .map(Request::Diagnostics)
                .map_err(|_| invalid)
        }
        "GetLog" if operation.payload_schema.as_str() == GET_LOG_REFERENCE_SCHEMA_16 => {
            serde_json::from_value(operation.payload.clone())
                .map(Request::Log)
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
            let (payload_schema, mut fields) = if action == "GetDiagnostics" {
                (
                    GET_DIAGNOSTICS_REFERENCE_SCHEMA_16,
                    vec![
                        field("startTime", ValueType::Text, false),
                        field("stopTime", ValueType::Text, false),
                    ],
                )
            } else {
                (
                    GET_LOG_REFERENCE_SCHEMA_16,
                    vec![
                        CommandSchemaField {
                            name: "logType",
                            value_type: ValueType::NamedEnum,
                            required: true,
                            enum_values: Some(vec!["DiagnosticsLog", "SecurityLog"]),
                        },
                        field("requestId", ValueType::SignedInteger, true),
                        field("oldestTimestamp", ValueType::Text, false),
                        field("latestTimestamp", ValueType::Text, false),
                    ],
                )
            };
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
