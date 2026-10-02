//! Pinned, explicitly supported privileged commands. Protocol edition is not a capability.
pub(crate) mod charging_profile16;
mod charging_profile16_schedule;
pub(crate) mod composite_schedule16;
pub(crate) mod device_model201;
mod trigger201;
use rust_ocpp::v1_6::messages::{
    change_availability::ChangeAvailabilityRequest, trigger_message::TriggerMessageRequest,
};
use serde::Serialize;
use serde_json::Value;
use uob_contracts::{
    CanonicalResource, CommandErrorCode, Connectivity, NativeProtocolReference, Operation,
    PrivilegedOcppOperation, ProtocolEdition, ResourceRef, StationSnapshot, ValueType,
};

const V16_SCHEMA: &str = "urn:OCPP:1.6:2019:12:ChangeAvailabilityRequest";
const V201_SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:ChangeAvailabilityRequest";
const V16_TRIGGER_SCHEMA: &str = "urn:OCPP:1.6:2019:12:TriggerMessageRequest";
const TRIGGER_VALUES: &[&str] = &[
    "BootNotification",
    "DiagnosticsStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "StatusNotification",
];

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
    let availability = Operation::ProtocolAction {
        protocol,
        action: "ChangeAvailability".to_owned(),
    };
    let mut descriptors = Vec::new();
    if snapshot.capabilities.supports(&availability)
        && let Some(descriptor) = availability_descriptor(snapshot, protocol)
    {
        descriptors.push(descriptor);
    }
    let trigger = Operation::ProtocolAction {
        protocol,
        action: "TriggerMessage".to_owned(),
    };
    if protocol == ProtocolEdition::Ocpp16j {
        descriptors.extend(charging_profile16::descriptors(snapshot));
        if snapshot.capabilities.supports(&trigger) && station_scope(&snapshot.station) {
            descriptors.push(trigger_descriptor(snapshot.station.clone()));
        }
        for connector in &snapshot.resources {
            if connector.capabilities.supports(&trigger)
                && connector_scope(&connector.resource).is_some()
                && connector.resource.bridge_id == snapshot.station.bridge_id
                && connector.resource.station_id == snapshot.station.station_id
            {
                descriptors.push(trigger_descriptor(connector.resource.clone()));
            }
        }
        let schedule = Operation::ProtocolAction {
            protocol,
            action: "GetCompositeSchedule".to_owned(),
        };
        if snapshot.capabilities.supports(&schedule)
            && composite_schedule16::connector(&snapshot.station).is_some()
        {
            descriptors.push(composite_schedule16::descriptor(snapshot.station.clone()));
        }
        for entry in &snapshot.resources {
            if entry.capabilities.supports(&schedule)
                && composite_schedule16::connector(&entry.resource).is_some()
                && entry.resource.bridge_id == snapshot.station.bridge_id
                && entry.resource.station_id == snapshot.station.station_id
            {
                descriptors.push(composite_schedule16::descriptor(entry.resource.clone()));
            }
        }
    } else {
        for (index, action) in device_model201::ACTIONS.iter().enumerate() {
            let operation = Operation::ProtocolAction {
                protocol,
                action: (*action).to_owned(),
            };
            if snapshot.capabilities.supports(&operation)
                && snapshot.station.native_protocol_reference.is_none()
            {
                descriptors.push(device_model201::descriptor(snapshot.station.clone(), index));
            }
            if index != 1 {
                for entry in &snapshot.resources {
                    if entry.capabilities.supports(&operation)
                        && entry.resource.bridge_id == snapshot.station.bridge_id
                        && entry.resource.station_id == snapshot.station.station_id
                    {
                        descriptors
                            .push(device_model201::descriptor(entry.resource.clone(), index));
                    }
                }
            }
        }
        if snapshot.capabilities.supports(&trigger) && trigger201::discoverable(&snapshot.station) {
            descriptors.push(trigger201_descriptor(snapshot.station.clone()));
        }
        for entry in &snapshot.resources {
            if entry.capabilities.supports(&trigger)
                && trigger201::discoverable(&entry.resource)
                && entry.resource.bridge_id == snapshot.station.bridge_id
                && entry.resource.station_id == snapshot.station.station_id
            {
                descriptors.push(trigger201_descriptor(entry.resource.clone()));
            }
        }
    }
    descriptors
}

fn availability_descriptor(
    snapshot: &StationSnapshot,
    protocol: ProtocolEdition,
) -> Option<CommandSchemaDescriptor> {
    let (schema, fields) = match protocol {
        ProtocolEdition::Ocpp16j => {
            if !matches!(
                snapshot.station.native_protocol_reference,
                None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
            ) {
                return None;
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
                return None;
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
    Some(CommandSchemaDescriptor {
        resource: snapshot.station.clone(),
        protocol,
        action: "ChangeAvailability",
        payload_schema: schema,
        fields,
    })
}

fn trigger_descriptor(resource: ResourceRef) -> CommandSchemaDescriptor {
    CommandSchemaDescriptor {
        resource,
        protocol: ProtocolEdition::Ocpp16j,
        action: "TriggerMessage",
        payload_schema: V16_TRIGGER_SCHEMA,
        fields: vec![
            CommandSchemaField {
                name: "requestedMessage",
                value_type: ValueType::NamedEnum,
                required: true,
                enum_values: Some(TRIGGER_VALUES.to_vec()),
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
fn trigger201_descriptor(resource: ResourceRef) -> CommandSchemaDescriptor {
    let (classes, child_scope) = match &resource.resource {
        None => (
            trigger201::CLASSES
                .iter()
                .copied()
                .filter(|class| *class != "StatusNotification")
                .collect(),
            None,
        ),
        Some(CanonicalResource::Evse {
            connector_id: None, ..
        }) => (
            vec![
                "MeterValues",
                "SignV2GCertificate",
                "TransactionEvent",
                "SignCombinedCertificate",
            ],
            Some(false),
        ),
        Some(CanonicalResource::Evse {
            connector_id: Some(_),
            ..
        }) => (vec!["StatusNotification", "TransactionEvent"], Some(true)),
        _ => unreachable!("only discoverable OCPP 2.0.1 resources have descriptors"),
    };
    let mut fields = vec![CommandSchemaField {
        name: "requestedMessage",
        value_type: ValueType::NamedEnum,
        required: true,
        enum_values: Some(classes),
    }];
    if let Some(connector_required) = child_scope {
        fields.push(CommandSchemaField {
            name: "evse.id",
            value_type: ValueType::UnsignedInteger,
            required: true,
            enum_values: None,
        });
        fields.push(CommandSchemaField {
            name: "evse.connectorId",
            value_type: ValueType::UnsignedInteger,
            required: connector_required,
            enum_values: None,
        });
    }

    CommandSchemaDescriptor {
        resource,
        protocol: ProtocolEdition::Ocpp201,
        action: "TriggerMessage",
        payload_schema: trigger201::SCHEMA,
        fields,
    }
}

fn station_scope(resource: &ResourceRef) -> bool {
    resource.resource.is_none()
        && matches!(
            resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
        )
}

fn connector_scope(resource: &ResourceRef) -> Option<u32> {
    match (&resource.resource, resource.native_protocol_reference) {
        (
            Some(CanonicalResource::Connector { .. }),
            Some(NativeProtocolReference::Ocpp16 { connector_id }),
        ) if connector_id > 0 => Some(connector_id),
        _ => None,
    }
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
    if device_model201::ACTIONS.contains(&operation.action.as_str()) {
        return device_model201::validate(resource, operation).map(|_| ());
    }
    if charging_profile16::ACTIONS.contains(&operation.action.as_str()) {
        return charging_profile16::validate(resource, operation).map(|_| ());
    }
    if operation.action.as_str() == "GetCompositeSchedule" {
        return composite_schedule16::validate(resource, operation).map(|_| ());
    }
    if operation.action.as_str() == "TriggerMessage" {
        return match operation.protocol {
            ProtocolEdition::Ocpp16j => validate_trigger(resource, operation),
            ProtocolEdition::Ocpp201 => trigger201::validate(resource, operation).map(|_| ()),
        };
    }
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

fn validate_trigger(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<(), CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp16j
        || operation.payload_schema.as_str() != V16_TRIGGER_SCHEMA
    {
        return Err(InvalidParameters);
    }

    let payload = operation.payload.as_object().ok_or(InvalidParameters)?;
    if payload.len() != 1 && payload.len() != 2 {
        return Err(InvalidParameters);
    }
    let class = payload
        .get("requestedMessage")
        .and_then(Value::as_str)
        .filter(|value| TRIGGER_VALUES.contains(value))
        .ok_or(InvalidParameters)?;
    let connector_id = match payload.get("connectorId") {
        Some(value) => Some(
            u32::try_from(value.as_u64().ok_or(InvalidParameters)?)
                .map_err(|_| InvalidParameters)?,
        ),
        None => None,
    };
    if payload.len() != 1 + usize::from(connector_id.is_some()) {
        return Err(InvalidParameters);
    }
    let _: TriggerMessageRequest =
        serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;

    match class {
        "MeterValues" | "StatusNotification" => match connector_id {
            Some(0) if class == "StatusNotification" && station_scope(resource) => Ok(()),
            Some(1..) if connector_scope(resource) == connector_id => Ok(()),
            None if station_scope(resource) => Ok(()),
            _ => Err(InvalidParameters),
        },
        // The message class is leading (§5.17): a supplied connectorId has no scope
        // effect for these four station-wide notifications.
        _ if station_scope(resource) => Ok(()),
        _ => Err(InvalidParameters),
    }
}
