use super::{CommandSchemaDescriptor, CommandSchemaField};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, Operation, PrivilegedOcppOperation, ProtocolEdition, ResourceRef,
    SEND_LOCAL_LIST_REFERENCE_SCHEMA_201, SendLocalListReference201, StationSnapshot, ValueType,
};

pub const ACTIONS: [&str; 3] = ["GetLocalListVersion", "SendLocalList", "ClearCache"];
pub const SCHEMAS: [&str; 3] = [
    "urn:OCPP:Cp:2:2020:3:GetLocalListVersionRequest",
    SEND_LOCAL_LIST_REFERENCE_SCHEMA_201,
    "urn:OCPP:Cp:2:2020:3:ClearCacheRequest",
];
fn station_scope(resource: &ResourceRef) -> bool {
    resource.resource.is_none()
        && matches!(
            resource.native_protocol_reference,
            None | Some(uob_contracts::NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
        )
}

pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Option<SendLocalListReference201>, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    let index = ACTIONS
        .iter()
        .position(|action| *action == operation.action.as_str())
        .ok_or(CommandErrorCode::UnsupportedOperation)?;
    if operation.protocol != ProtocolEdition::Ocpp201
        || !station_scope(resource)
        || operation.payload_schema.as_str() != SCHEMAS[index]
    {
        return Err(invalid);
    }
    if index == 1 {
        let request: SendLocalListReference201 =
            serde_json::from_value(operation.payload.clone()).map_err(|_| invalid)?;
        if !request.valid() {
            return Err(invalid);
        }
        Ok(Some(request))
    } else if operation
        .payload
        .as_object()
        .is_some_and(serde_json::Map::is_empty)
    {
        Ok(None)
    } else {
        Err(invalid)
    }
}

pub fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    if !station_scope(&snapshot.station) {
        return vec![];
    }
    ACTIONS
        .iter()
        .enumerate()
        .filter_map(|(index, action)| {
            let operation = Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: (*action).to_owned(),
            };
            if !snapshot.capabilities.supports(&operation) {
                return None;
            }
            let fields = if index == 1 {
                vec![
                    CommandSchemaField {
                        name: "versionNumber",
                        value_type: ValueType::SignedInteger,
                        required: true,
                        enum_values: None,
                    },
                    CommandSchemaField {
                        name: "updateType",
                        value_type: ValueType::NamedEnum,
                        required: true,
                        enum_values: Some(vec!["Full", "Differential"]),
                    },
                    CommandSchemaField {
                        name: "updateReference",
                        value_type: ValueType::Text,
                        required: true,
                        enum_values: None,
                    },
                ]
            } else {
                vec![]
            };
            Some(CommandSchemaDescriptor {
                resource: snapshot.station.clone(),
                protocol: ProtocolEdition::Ocpp201,
                action,
                payload_schema: SCHEMAS[index],
                fields,
            })
        })
        .collect()
}
