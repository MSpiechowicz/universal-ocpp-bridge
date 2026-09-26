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

const TRIGGER_SCHEMA: &str = "urn:OCPP:1.6:2019:12:TriggerMessageRequest";
const TRIGGER_CLASSES: [&str; 6] = [
    "BootNotification",
    "DiagnosticsStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "StatusNotification",
];

fn trigger_operation(class: &str, connector_id: Option<Value>) -> PrivilegedOcppOperation<Value> {
    let mut payload = json!({"requestedMessage":class});
    if let Some(connector_id) = connector_id {
        payload["connectorId"] = connector_id;
    }
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("TriggerMessage").unwrap(),
        payload_schema: PayloadSchemaId::new(TRIGGER_SCHEMA).unwrap(),
        payload,
    }
}

#[test]
fn trigger_descriptors_require_local_capability_and_correct_connected_edition() {
    let mut snapshot = snapshot(ProtocolEdition::Ocpp16j);
    let trigger_capability = SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "TriggerMessage".to_owned(),
        },
        parameters: vec![],
    };
    assert!(command_schemas(&snapshot).is_empty());

    snapshot.resources[0]
        .capabilities
        .operations
        .push(trigger_capability.clone());
    let descriptors = command_schemas(&snapshot);
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].resource, snapshot.resources[0].resource);
    assert_eq!(descriptors[0].payload_schema, TRIGGER_SCHEMA);
    assert_eq!(
        descriptors[0].fields[0].enum_values.as_deref(),
        Some(TRIGGER_CLASSES.as_slice())
    );
    assert!(!descriptors[0].fields[1].required);
    assert_eq!(descriptors[0].fields[1].name, "connectorId");

    snapshot.capabilities.operations.push(trigger_capability);
    let descriptors = command_schemas(&snapshot);
    assert_eq!(descriptors.len(), 2);
    assert_eq!(descriptors[0].resource, snapshot.station);
    assert_eq!(descriptors[1].resource, snapshot.resources[0].resource);
    assert!(
        descriptors
            .iter()
            .all(|descriptor| descriptor.action == "TriggerMessage")
    );

    snapshot.connectivity = Connectivity::Disconnected;
    assert!(command_schemas(&snapshot).is_empty());
    snapshot.connectivity = Connectivity::Connected {
        protocol: ProtocolEdition::Ocpp201,
        connected_at: snapshot.observed_at,
        last_message_at: None,
    };
    assert!(command_schemas(&snapshot).is_empty());
}

#[test]
fn trigger_class_and_scope_enforce_exact_connector_or_station_authority() {
    let snapshot = snapshot(ProtocolEdition::Ocpp16j);
    let station = &snapshot.station;
    let connector = &snapshot.resources[0].resource;
    for class in TRIGGER_CLASSES {
        let request = trigger_operation(class, None);
        assert_eq!(
            validate_privileged_operation(station, &request),
            Ok(()),
            "{class}"
        );
        assert_eq!(
            validate_privileged_operation(connector, &request),
            Err(CommandErrorCode::InvalidParameters),
            "{class}"
        );
    }

    for class in ["StatusNotification", "MeterValues"] {
        let request = trigger_operation(class, Some(json!(1)));
        assert_eq!(validate_privileged_operation(connector, &request), Ok(()));
        assert_eq!(
            validate_privileged_operation(station, &request),
            Err(CommandErrorCode::InvalidParameters)
        );
        let request = trigger_operation(class, Some(json!(2)));
        assert_eq!(
            validate_privileged_operation(connector, &request),
            Err(CommandErrorCode::InvalidParameters)
        );
    }
    assert_eq!(
        validate_privileged_operation(
            station,
            &trigger_operation("StatusNotification", Some(json!(0)))
        ),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(station, &trigger_operation("MeterValues", Some(json!(0)))),
        Err(CommandErrorCode::InvalidParameters)
    );

    for class in &TRIGGER_CLASSES[..4] {
        let irrelevant = trigger_operation(class, Some(json!(2)));
        assert_eq!(validate_privileged_operation(station, &irrelevant), Ok(()));
        assert_eq!(
            validate_privileged_operation(connector, &irrelevant),
            Err(CommandErrorCode::InvalidParameters)
        );
    }
}

#[test]
fn trigger_registry_rejects_invalid_schema_fields_and_foreign_resources() {
    let snapshot = snapshot(ProtocolEdition::Ocpp16j);
    let station = &snapshot.station;
    let valid = trigger_operation("Heartbeat", None);
    let mut invalid = valid.clone();
    invalid.payload_schema = PayloadSchemaId::new("foreign").unwrap();
    assert_eq!(
        validate_privileged_operation(station, &invalid),
        Err(CommandErrorCode::InvalidParameters)
    );
    invalid = valid.clone();
    invalid.protocol = ProtocolEdition::Ocpp201;
    assert_eq!(
        validate_privileged_operation(station, &invalid),
        Err(CommandErrorCode::InvalidParameters)
    );
    for payload in [
        json!({}),
        json!({"requestedMessage":"Reset"}),
        json!({"requestedMessage":"Heartbeat","unknown":true}),
        json!({"requestedMessage":"Heartbeat","connectorId":null}),
        json!({"requestedMessage":"Heartbeat","connectorId":-1}),
        json!({"requestedMessage":"Heartbeat","connectorId":4_294_967_296_u64}),
        json!({"requestedMessage":"StatusNotification","connectorId":1.0}),
    ] {
        invalid = valid.clone();
        invalid.payload = payload.clone();
        assert_eq!(
            validate_privileged_operation(station, &invalid),
            Err(CommandErrorCode::InvalidParameters),
            "{payload}"
        );
    }
    let mut foreign = snapshot.resources[0].resource.clone();
    foreign.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    assert_eq!(
        validate_privileged_operation(
            &foreign,
            &trigger_operation("StatusNotification", Some(json!(1)))
        ),
        Err(CommandErrorCode::InvalidParameters)
    );
}
