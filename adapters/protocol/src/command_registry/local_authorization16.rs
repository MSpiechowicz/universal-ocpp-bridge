use super::{CommandSchemaDescriptor, CommandSchemaField, station_scope};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, Operation, PrivilegedOcppOperation, ProtocolEdition, ResourceRef,
    SEND_LOCAL_LIST_REFERENCE_SCHEMA_16, SendLocalListReference16, StationSnapshot, ValueType,
};

pub const ACTIONS: [&str; 3] = ["GetLocalListVersion", "SendLocalList", "ClearCache"];
pub const SCHEMAS: [&str; 3] = [
    "urn:OCPP:1.6:2019:12:GetLocalListVersionRequest",
    SEND_LOCAL_LIST_REFERENCE_SCHEMA_16,
    "urn:OCPP:1.6:2019:12:ClearCacheRequest",
];

pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Option<SendLocalListReference16>, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    let index = ACTIONS
        .iter()
        .position(|action| *action == operation.action.as_str())
        .ok_or(CommandErrorCode::UnsupportedOperation)?;
    if operation.protocol != ProtocolEdition::Ocpp16j
        || !station_scope(resource)
        || operation.payload_schema.as_str() != SCHEMAS[index]
    {
        return Err(invalid);
    }
    if index == 1 {
        let request: SendLocalListReference16 =
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
                protocol: ProtocolEdition::Ocpp16j,
                action: (*action).to_owned(),
            };
            if !snapshot.capabilities.supports(&operation) {
                return None;
            }
            let fields = if index == 1 {
                vec![
                    CommandSchemaField {
                        name: "listVersion",
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
                protocol: ProtocolEdition::Ocpp16j,
                action,
                payload_schema: SCHEMAS[index],
                fields,
            })
        })
        .collect()
}
