use super::snapshot;
use serde_json::{Value, json};
use uob_contracts::{
    CommandErrorCode, Connectivity, NativeProtocolReference, Operation, PayloadSchemaId,
    PrivilegedOcppOperation, ProtocolActionName, ProtocolEdition, ResourceRef, SupportedOperation,
};
use uob_protocol_adapter::command_registry::{command_schemas, validate_privileged_operation};

const TRIGGER_201_SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:TriggerMessageRequest";
const TRIGGER_201_CLASSES: [&str; 11] = [
    "BootNotification",
    "LogStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "SignChargingStationCertificate",
    "SignV2GCertificate",
    "StatusNotification",
    "TransactionEvent",
    "SignCombinedCertificate",
    "PublishFirmwareStatusNotification",
];

fn trigger_201(class: &str, evse: Option<Value>) -> PrivilegedOcppOperation<Value> {
    let mut payload = json!({"requestedMessage":class});
    if let Some(evse) = evse {
        payload["evse"] = evse;
    }
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("TriggerMessage").unwrap(),
        payload_schema: PayloadSchemaId::new(TRIGGER_201_SCHEMA).unwrap(),
        payload,
    }
}

#[test]
fn trigger_201_discovery_requires_exact_local_capability_and_native_scope() {
    let mut snapshot = snapshot(ProtocolEdition::Ocpp201);
    let capability = SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: "TriggerMessage".to_owned(),
        },
        parameters: vec![],
    };
    assert!(command_schemas(&snapshot).is_empty());
    snapshot.resources[0]
        .capabilities
        .operations
        .push(capability.clone());
    let descriptors = command_schemas(&snapshot);
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].resource, snapshot.resources[0].resource);
    assert_eq!(
        descriptors[0].fields[0].enum_values.as_deref(),
        Some(["StatusNotification", "TransactionEvent"].as_slice())
    );
    assert_eq!(descriptors[0].payload_schema, TRIGGER_201_SCHEMA);

    snapshot.capabilities.operations.push(capability);
    assert_eq!(command_schemas(&snapshot).len(), 2);
    snapshot.connectivity = Connectivity::Disconnected;
    assert!(command_schemas(&snapshot).is_empty());
}

#[test]
fn trigger_201_discovery_only_offers_valid_classes_and_requires_native_child_scope() {
    let mut snapshot = snapshot(ProtocolEdition::Ocpp201);
    let capability = SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: "TriggerMessage".to_owned(),
        },
        parameters: vec![],
    };
    snapshot.capabilities.operations.push(capability.clone());
    snapshot.resources[0]
        .capabilities
        .operations
        .push(capability);
    let mut evse = snapshot.resources[0].clone();
    if let Some(uob_contracts::CanonicalResource::Evse { connector_id, .. }) =
        &mut evse.resource.resource
    {
        *connector_id = None;
    }
    evse.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    snapshot.resources.push(evse);

    let descriptors = command_schemas(&snapshot);
    assert_eq!(descriptors.len(), 3);
    for (descriptor, resource, expected_classes, expected_fields) in [
        (
            &descriptors[0],
            &snapshot.station,
            TRIGGER_201_CLASSES
                .iter()
                .copied()
                .filter(|class| *class != "StatusNotification")
                .collect::<Vec<_>>(),
            vec![("requestedMessage", true)],
        ),
        (
            &descriptors[1],
            &snapshot.resources[0].resource,
            vec!["StatusNotification", "TransactionEvent"],
            vec![
                ("requestedMessage", true),
                ("evse.id", true),
                ("evse.connectorId", true),
            ],
        ),
        (
            &descriptors[2],
            &snapshot.resources[3].resource,
            vec![
                "MeterValues",
                "SignV2GCertificate",
                "TransactionEvent",
                "SignCombinedCertificate",
            ],
            vec![
                ("requestedMessage", true),
                ("evse.id", true),
                ("evse.connectorId", false),
            ],
        ),
    ] {
        assert_eq!(&descriptor.resource, resource);
        assert_eq!(descriptor.action, "TriggerMessage");
        assert_eq!(descriptor.payload_schema, TRIGGER_201_SCHEMA);
        let classes = descriptor.fields[0].enum_values.as_deref().unwrap();
        assert_eq!(classes, expected_classes, "{resource:?}");
        assert_eq!(
            descriptor
                .fields
                .iter()
                .map(|field| (field.name, field.required))
                .collect::<Vec<_>>(),
            expected_fields,
            "{resource:?}"
        );

        assert_201_admission_matches_discovery(resource, classes);
    }
}

fn assert_201_admission_matches_discovery(resource: &ResourceRef, classes: &[&str]) {
    let native_scope = match resource.native_protocol_reference {
        Some(NativeProtocolReference::Ocpp201 {
            evse_id,
            connector_id: Some(connector_id),
        }) => Some(json!({"id":evse_id,"connectorId":connector_id})),
        Some(NativeProtocolReference::Ocpp201 {
            evse_id,
            connector_id: None,
        }) => Some(json!({"id":evse_id})),
        _ => None,
    };
    for class in TRIGGER_201_CLASSES {
        let accepted =
            validate_privileged_operation(resource, &trigger_201(class, native_scope.clone()))
                .is_ok();
        assert_eq!(
            classes.contains(&class),
            accepted,
            "{class} advertised for {resource:?} must match native scope validation"
        );
        if classes.contains(&class) && native_scope.is_some() {
            assert_eq!(
                validate_privileged_operation(resource, &trigger_201(class, None)),
                Err(CommandErrorCode::InvalidParameters),
                "{class} on a child requires evse.id"
            );
            if let Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id: Some(_),
            }) = resource.native_protocol_reference
            {
                assert_eq!(
                    validate_privileged_operation(
                        resource,
                        &trigger_201(class, Some(json!({"id":evse_id})))
                    ),
                    Err(CommandErrorCode::InvalidParameters),
                    "{class} on a connector requires evse.connectorId"
                );
            }
        }
    }
}

fn assert_201_invalid_payloads(station: &ResourceRef, connector: &ResourceRef, evse: &ResourceRef) {
    for payload in [
        json!({"requestedMessage":"NotAClass"}),
        json!({"requestedMessage":"Heartbeat","extra":0}),
        json!({"requestedMessage":"StatusNotification","evse":{"id":1}}),
        json!({"requestedMessage":"StatusNotification","evse":{"id":1,"connectorId":0}}),
        json!({"requestedMessage":"StatusNotification","evse":{"id":1,"connectorId":2}}),
        json!({"requestedMessage":"MeterValues","evse":{"id":0}}),
        json!({"requestedMessage":"MeterValues","evse":{"id":2_147_483_648_u64}}),
        json!({"requestedMessage":"MeterValues","evse":{"id":1,"unknown":false}}),
    ] {
        let mut request = trigger_201("Heartbeat", None);
        request.payload = payload.clone();
        let resource = match payload["requestedMessage"].as_str() {
            Some("StatusNotification") => connector,
            Some("MeterValues") => evse,
            _ => station,
        };
        assert_eq!(
            validate_privileged_operation(resource, &request),
            Err(CommandErrorCode::InvalidParameters),
            "{payload}"
        );
    }
}

#[test]
fn trigger_201_edition_and_scope_deny_unknown_fields_and_widening() {
    let snapshot = snapshot(ProtocolEdition::Ocpp201);
    let station = &snapshot.station;
    let connector = &snapshot.resources[0].resource;
    let mut evse = connector.clone();
    if let Some(uob_contracts::CanonicalResource::Evse { connector_id, .. }) = &mut evse.resource {
        *connector_id = None;
    }
    evse.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });

    for class in TRIGGER_201_CLASSES {
        let request = trigger_201(class, None);
        assert_eq!(
            validate_privileged_operation(station, &request),
            if class == "StatusNotification" {
                Err(CommandErrorCode::InvalidParameters)
            } else {
                Ok(())
            },
            "{class}"
        );
    }
    for (class, resource, scope) in [
        ("MeterValues", &evse, json!({"id":1,"connectorId":1})),
        (
            "TransactionEvent",
            connector,
            json!({"id":1,"connectorId":1}),
        ),
        (
            "StatusNotification",
            connector,
            json!({"id":1,"connectorId":1}),
        ),
        ("SignV2GCertificate", &evse, json!({"id":1})),
    ] {
        assert_eq!(
            validate_privileged_operation(resource, &trigger_201(class, Some(scope))),
            Ok(()),
            "{class}"
        );
    }
    assert_eq!(
        validate_privileged_operation(
            station,
            &trigger_201("SignChargingStationCertificate", Some(json!({"id":1})))
        ),
        Ok(()),
        "irrelevant EVSE is ignored for station certificate, without widening station authority"
    );
    assert_eq!(
        validate_privileged_operation(
            &evse,
            &trigger_201("SignChargingStationCertificate", Some(json!({"id":1})))
        ),
        Err(CommandErrorCode::InvalidParameters)
    );
    assert_eq!(
        validate_privileged_operation(
            connector,
            &trigger_201("MeterValues", Some(json!({"id":1,"connectorId":1})))
        ),
        Err(CommandErrorCode::InvalidParameters),
        "connector authorization cannot widen to EVSE-wide meter report"
    );
    assert_201_invalid_payloads(station, connector, &evse);
    let mut foreign = trigger_201("Heartbeat", None);
    foreign.payload_schema =
        PayloadSchemaId::new("urn:OCPP:Cp:2:2020:2:TriggerMessageRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(station, &foreign),
        Err(CommandErrorCode::InvalidParameters)
    );
    foreign = trigger_201("Heartbeat", None);
    foreign.protocol = ProtocolEdition::Ocpp16j;
    assert_eq!(
        validate_privileged_operation(station, &foreign),
        Err(CommandErrorCode::InvalidParameters)
    );
    let valid_custom = trigger_201(
        "MeterValues",
        Some(json!({"id":1,"customData":{"vendorId":"manufacturer"}})),
    );
    assert_eq!(validate_privileged_operation(&evse, &valid_custom), Ok(()));
    let invalid_custom = trigger_201("Heartbeat", None);
    let mut invalid_custom = invalid_custom;
    invalid_custom.payload["customData"] = json!({"extra":true});
    assert_eq!(
        validate_privileged_operation(station, &invalid_custom),
        Err(CommandErrorCode::InvalidParameters)
    );
}
