//! OCPP 1.6 Security Whitepaper `ExtendedTriggerMessage` (section 5.5). Scope follows plain
//! `TriggerMessage`, but a supplied connectorId must be positive and the two Whitepaper-only
//! classes exist. The action is offered only where the station explicitly advertises it.
use super::{CommandSchemaDescriptor, CommandSchemaField, connector_scope, station_scope};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, Operation, PrivilegedOcppOperation, ProtocolEdition, ResourceCapabilities,
    ResourceRef, StationSnapshot, TriggerMessageClass, ValueType,
};

pub const ACTION: &str = "ExtendedTriggerMessage";
pub const SCHEMA: &str = "urn:OCPP:Cp:1.6:2020:3:ExtendedTriggerMessage.req";

/// Requested names the bridge can send, in descriptor order, with the class each one produces.
/// An extended `FirmwareStatusNotification` request yields the signed-firmware class, which the
/// plain firmware notification never satisfies.
const CLASSES: [(&str, TriggerMessageClass); 6] = [
    ("BootNotification", TriggerMessageClass::BootNotification),
    (
        "LogStatusNotification",
        TriggerMessageClass::LogStatusNotification,
    ),
    (
        "FirmwareStatusNotification",
        TriggerMessageClass::SignedFirmwareStatusNotification,
    ),
    ("Heartbeat", TriggerMessageClass::Heartbeat),
    ("MeterValues", TriggerMessageClass::MeterValues),
    (
        "StatusNotification",
        TriggerMessageClass::StatusNotification,
    ),
];

/// In the pinned schema enum, but triggering `SignCertificate` belongs to certificate management.
const SIGN_CERTIFICATE: &str = "SignChargePointCertificate";

/// A request that passed the exact schema and scope rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub class: TriggerMessageClass,
    pub connector_id: Option<u32>,
}

/// Whether an operation belongs to this module; 2.0.1 has no such action.
#[must_use]
pub fn owns(operation: &PrivilegedOcppOperation<Value>) -> bool {
    operation.action.as_str() == ACTION && operation.protocol == ProtocolEdition::Ocpp16j
}

/// Whether the station advertises the operation that the class's reply belongs to.
#[must_use]
pub fn class_available(station: &ResourceCapabilities, class: TriggerMessageClass) -> bool {
    let enabling = match class {
        TriggerMessageClass::LogStatusNotification => "GetLog",
        TriggerMessageClass::SignedFirmwareStatusNotification => "SignedUpdateFirmware",
        _ => return true,
    };
    station.supports(&Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp16j,
        action: enabling.to_owned(),
    })
}

/// Validates the exact request schema and scope; capability and grant stay separate checks.
///
/// # Errors
/// `UnsupportedOperation` for `SignChargePointCertificate`; `InvalidParameters` for every
/// schema, field, connector or scope mismatch.
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    use CommandErrorCode::{InvalidParameters, UnsupportedOperation};
    if operation.protocol != ProtocolEdition::Ocpp16j || operation.payload_schema.as_str() != SCHEMA
    {
        return Err(InvalidParameters);
    }
    let payload = operation.payload.as_object().ok_or(InvalidParameters)?;
    if payload
        .keys()
        .any(|key| key != "requestedMessage" && key != "connectorId")
    {
        return Err(InvalidParameters);
    }
    let name = payload
        .get("requestedMessage")
        .and_then(Value::as_str)
        .ok_or(InvalidParameters)?;
    if name == SIGN_CERTIFICATE {
        return Err(UnsupportedOperation);
    }
    let class = CLASSES
        .iter()
        .find_map(|(requested, class)| (*requested == name).then_some(*class))
        .ok_or(InvalidParameters)?;
    let connector_id = match payload.get("connectorId") {
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .filter(|id| *id > 0)
                .ok_or(InvalidParameters)?,
        ),
        None => None,
    };
    let in_scope = if class.is_station_only() {
        // The message class is leading: a supplied connectorId has no scope effect here.
        station_scope(resource)
    } else {
        match connector_id {
            Some(id) => connector_scope(resource) == Some(id),
            None => station_scope(resource),
        }
    };
    if !in_scope {
        return Err(InvalidParameters);
    }
    Ok(Request {
        class,
        connector_id,
    })
}

fn descriptor(resource: ResourceRef, values: Vec<&'static str>) -> CommandSchemaDescriptor {
    CommandSchemaDescriptor {
        resource,
        protocol: ProtocolEdition::Ocpp16j,
        action: ACTION,
        payload_schema: SCHEMA,
        fields: vec![
            CommandSchemaField {
                name: "requestedMessage",
                value_type: ValueType::NamedEnum,
                required: true,
                enum_values: Some(values),
            },
            CommandSchemaField {
                name: "connectorId",
                value_type: ValueType::UnsignedInteger,
                required: false,
                enum_values: None,
            },
        ],
    }
}

/// Offers the action on the station root and on every admitted connector that advertises it.
/// Each resource lists only the classes it can address, and station-only classes appear only
/// while the station advertises the operation that produces them.
#[must_use]
pub fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let operation = Operation::ProtocolAction {
        protocol: ProtocolEdition::Ocpp16j,
        action: ACTION.to_owned(),
    };
    let mut descriptors = Vec::new();
    if snapshot.capabilities.supports(&operation) && station_scope(&snapshot.station) {
        let values = CLASSES
            .iter()
            .filter(|(_, class)| class_available(&snapshot.capabilities, *class))
            .map(|(name, _)| *name)
            .collect();
        descriptors.push(descriptor(snapshot.station.clone(), values));
    }
    for entry in &snapshot.resources {
        if entry.capabilities.supports(&operation)
            && connector_scope(&entry.resource).is_some()
            && entry.resource.bridge_id == snapshot.station.bridge_id
            && entry.resource.station_id == snapshot.station.station_id
        {
            descriptors.push(descriptor(
                entry.resource.clone(),
                vec!["MeterValues", "StatusNotification"],
            ));
        }
    }
    descriptors
}
