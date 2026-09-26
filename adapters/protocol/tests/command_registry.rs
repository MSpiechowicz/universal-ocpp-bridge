use serde_json::{Value, json};
use uob_contracts::{
    CommandErrorCode, Connectivity, NativeProtocolReference, Operation, PayloadSchemaId,
    PrivilegedOcppOperation, ProtocolActionName, ProtocolEdition, StationSnapshot,
    SupportedOperation,
};
use uob_protocol_adapter::command_registry::{command_schemas, validate_privileged_operation};

fn snapshot(protocol: ProtocolEdition) -> StationSnapshot {
    let bytes: &[u8] = match protocol {
        ProtocolEdition::Ocpp16j => include_bytes!(
            "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
        ),
        ProtocolEdition::Ocpp201 => include_bytes!(
            "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
        ),
    };
    let mut snapshot: StationSnapshot = serde_json::from_slice(bytes).unwrap();
    snapshot.connectivity = Connectivity::Connected {
        protocol,
        connected_at: snapshot.observed_at,
        last_message_at: None,
    };
    snapshot.station.native_protocol_reference = None;
    snapshot
}

#[test]
fn descriptors_require_explicit_station_capability_not_edition_or_connector_capability() {
    for protocol in [ProtocolEdition::Ocpp16j, ProtocolEdition::Ocpp201] {
        let mut snapshot = snapshot(protocol);
        assert!(command_schemas(&snapshot).is_empty());
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol,
                action: "ChangeAvailability".to_owned(),
            },
            parameters: vec![],
        });
        let [descriptor] = command_schemas(&snapshot).try_into().unwrap();
        assert_eq!(descriptor.resource, snapshot.station);
        assert_eq!(descriptor.action, "ChangeAvailability");
        let value = serde_json::to_value(descriptor).unwrap();
        assert_eq!(
            value["fields"].as_array().unwrap().len(),
            if protocol == ProtocolEdition::Ocpp16j {
                2
            } else {
                1
            }
        );
        snapshot.connectivity = Connectivity::Disconnected;
        assert!(command_schemas(&snapshot).is_empty());
    }
}

#[test]
fn privileged_schema_rejects_widening_unknown_fields_and_foreign_protocol() {
    for protocol in [ProtocolEdition::Ocpp16j, ProtocolEdition::Ocpp201] {
        let snapshot = snapshot(protocol);
        let descriptor = command_schemas(&{
            let mut snapshot = snapshot.clone();
            snapshot.capabilities.operations.push(SupportedOperation {
                operation: Operation::ProtocolAction {
                    protocol,
                    action: "ChangeAvailability".to_owned(),
                },
                parameters: vec![],
            });
            snapshot
        })
        .remove(0);
        let mut operation = PrivilegedOcppOperation {
            protocol,
            action: ProtocolActionName::new(descriptor.action).unwrap(),
            payload_schema: PayloadSchemaId::new(descriptor.payload_schema).unwrap(),
            payload: if protocol == ProtocolEdition::Ocpp16j {
                json!({"connectorId":0,"type":"Inoperative"})
            } else {
                json!({"operationalStatus":"Inoperative"})
            },
        };
        assert_eq!(
            validate_privileged_operation(&snapshot.station, &operation),
            Ok(())
        );
        let original = operation.payload.clone();
        operation.payload["unexpected"] = Value::Bool(true);
        assert_eq!(
            validate_privileged_operation(&snapshot.station, &operation),
            Err(CommandErrorCode::InvalidParameters)
        );
        operation.payload = original;
        let mut child = snapshot.resources[0].resource.clone();
        child.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 { connector_id: 0 });
        assert_eq!(
            validate_privileged_operation(&child, &operation),
            Err(CommandErrorCode::InvalidParameters)
        );
        operation.protocol = if protocol == ProtocolEdition::Ocpp16j {
            ProtocolEdition::Ocpp201
        } else {
            ProtocolEdition::Ocpp16j
        };
        assert_eq!(
            validate_privileged_operation(&snapshot.station, &operation),
            Err(CommandErrorCode::InvalidParameters)
        );
        operation.protocol = protocol;
        operation.payload_schema = PayloadSchemaId::new("foreign").unwrap();
        assert_eq!(
            validate_privileged_operation(&snapshot.station, &operation),
            Err(CommandErrorCode::InvalidParameters)
        );
    }
}
