use super::*;

const SCHEMA: &str = "urn:OCPP:Cp:1.6:2020:3:ExtendedTriggerMessage.req";

fn corpus(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/1.6")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn extended(payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("ExtendedTriggerMessage").unwrap(),
        payload_schema: PayloadSchemaId::new(SCHEMA).unwrap(),
        payload,
    }
}

fn advertise(operations: &mut Vec<SupportedOperation>, action: &str) {
    operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: action.to_owned(),
        },
        parameters: vec![],
    });
}

fn enabled() -> StationSnapshot {
    let mut state = snapshot(ProtocolEdition::Ocpp16j);
    advertise(&mut state.capabilities.operations, "ExtendedTriggerMessage");
    for entry in &mut state.resources {
        advertise(&mut entry.capabilities.operations, "ExtendedTriggerMessage");
    }
    state
}

fn values(
    descriptor: &uob_protocol_adapter::command_registry::CommandSchemaDescriptor,
) -> Vec<String> {
    serde_json::to_value(descriptor).unwrap()["fields"][0]["enum_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn descriptor_requires_the_station_option_on_each_resource() {
    let mut state = snapshot(ProtocolEdition::Ocpp16j);
    assert!(command_schemas(&state).is_empty());
    advertise(
        &mut state.resources[0].capabilities.operations,
        "ExtendedTriggerMessage",
    );
    let [connector] = command_schemas(&state).try_into().unwrap();
    assert_eq!(connector.resource, state.resources[0].resource);
    advertise(&mut state.capabilities.operations, "ExtendedTriggerMessage");
    let found = command_schemas(&state);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].resource, state.station);

    let state = enabled();
    let found = command_schemas(&state);
    assert_eq!(found.len(), 3);
    for descriptor in &found {
        assert_eq!(descriptor.action, "ExtendedTriggerMessage");
        assert_eq!(descriptor.payload_schema, SCHEMA);
        assert_eq!(descriptor.protocol, ProtocolEdition::Ocpp16j);
        let fields = serde_json::to_value(descriptor).unwrap()["fields"].clone();
        assert_eq!(fields[0]["name"], "requestedMessage");
        assert_eq!(fields[0]["required"], true);
        assert_eq!(fields[1]["name"], "connectorId");
        assert_eq!(fields[1]["required"], false);
    }

    let mut other = enabled();
    other.connectivity = Connectivity::Disconnected;
    assert!(command_schemas(&other).is_empty());
    let mut other = enabled();
    other.connectivity = Connectivity::Connected {
        protocol: ProtocolEdition::Ocpp201,
        connected_at: other.observed_at,
        last_message_at: None,
    };
    assert!(
        command_schemas(&other)
            .iter()
            .all(|descriptor| descriptor.action != "ExtendedTriggerMessage")
    );
}

#[test]
fn requested_values_follow_the_capabilities_that_produce_the_reply() {
    let mut state = enabled();
    let base = [
        "BootNotification",
        "Heartbeat",
        "MeterValues",
        "StatusNotification",
    ];
    let found = command_schemas(&state);
    assert_eq!(values(&found[0]), base);
    assert_eq!(values(&found[1]), ["MeterValues", "StatusNotification"]);
    assert!(!values(&found[0]).contains(&"SignChargePointCertificate".to_owned()));

    advertise(&mut state.capabilities.operations, "GetLog");
    let found = command_schemas(&state);
    let station = found
        .iter()
        .find(|descriptor| {
            descriptor.resource == state.station && descriptor.action == "ExtendedTriggerMessage"
        })
        .unwrap();
    assert_eq!(
        values(station),
        [
            "BootNotification",
            "LogStatusNotification",
            "Heartbeat",
            "MeterValues",
            "StatusNotification"
        ]
    );

    advertise(&mut state.capabilities.operations, "SignedUpdateFirmware");
    let found = command_schemas(&state);
    let station = found
        .iter()
        .find(|descriptor| {
            descriptor.resource == state.station && descriptor.action == "ExtendedTriggerMessage"
        })
        .unwrap();
    assert_eq!(
        values(station),
        [
            "BootNotification",
            "LogStatusNotification",
            "FirmwareStatusNotification",
            "Heartbeat",
            "MeterValues",
            "StatusNotification"
        ]
    );

    // A connector's own advertisement of the enabling operations never widens the station list.
    let mut state = enabled();
    advertise(&mut state.resources[0].capabilities.operations, "GetLog");
    let found = command_schemas(&state);
    assert_eq!(values(&found[0]), base);
}

#[test]
fn every_corpus_request_validates_on_its_native_scope() {
    let state = enabled();
    let connector = &state.resources[0].resource;
    let mut checked = 0;
    for entry in std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ocpp-fixtures/corpus/wire/1.6"),
    )
    .unwrap()
    {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.starts_with("extended-trigger-") {
            continue;
        }
        let frame = corpus(&name);
        if frame[0] != 2 {
            continue;
        }
        let payload = frame[3].clone();
        let resource = if payload["requestedMessage"]
            .as_str()
            .is_some_and(|class| ["MeterValues", "StatusNotification"].contains(&class))
            && payload.get("connectorId").is_some()
        {
            connector
        } else {
            &state.station
        };
        assert_eq!(
            validate_privileged_operation(resource, &extended(payload)),
            Ok(()),
            "{name}"
        );
        checked += 1;
    }
    assert_eq!(checked, 8);
}

#[test]
fn negative_corpus_cases_are_refused_with_their_documented_code() {
    let state = enabled();
    let cases = corpus("security-negative-cases.json");
    let (mut schema, mut application) = (0, 0);
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][0] != 2 || case["wire"][2] != "ExtendedTriggerMessage" {
            continue;
        }
        let expected = if case["level"] == "application" {
            application += 1;
            match case["bridge_refusal"].as_str().unwrap() {
                "UnsupportedOperation" => CommandErrorCode::UnsupportedOperation,
                "InvalidParameters" => CommandErrorCode::InvalidParameters,
                other => panic!("unmapped refusal {other}"),
            }
        } else {
            schema += 1;
            CommandErrorCode::InvalidParameters
        };
        for resource in [&state.station, &state.resources[0].resource] {
            let result =
                validate_privileged_operation(resource, &extended(case["wire"][3].clone()));
            // Every documented refusal holds whatever resource the command addresses.
            assert_eq!(result, Err(expected), "{} on {resource:?}", case["id"]);
        }
    }
    assert_eq!((schema, application), (8, 5));
}

#[test]
fn scope_follows_plain_trigger_but_requires_positive_connector_ids() {
    let state = enabled();
    let station = &state.station;
    let connector = &state.resources[0].resource;
    let invalid = Err(CommandErrorCode::InvalidParameters);
    for class in [
        "BootNotification",
        "LogStatusNotification",
        "FirmwareStatusNotification",
        "Heartbeat",
    ] {
        assert_eq!(
            validate_privileged_operation(station, &extended(json!({"requestedMessage": class}))),
            Ok(()),
            "{class}"
        );
        assert_eq!(
            validate_privileged_operation(connector, &extended(json!({"requestedMessage": class}))),
            invalid,
            "{class}"
        );
        // The class is leading: a positive connectorId has no scope effect on station messages.
        assert_eq!(
            validate_privileged_operation(
                station,
                &extended(json!({"requestedMessage": class, "connectorId": 2}))
            ),
            Ok(()),
            "{class}"
        );
        assert_eq!(
            validate_privileged_operation(
                station,
                &extended(json!({"requestedMessage": class, "connectorId": 0}))
            ),
            invalid,
            "{class}"
        );
    }
    for class in ["MeterValues", "StatusNotification"] {
        let all = extended(json!({"requestedMessage": class}));
        assert_eq!(validate_privileged_operation(station, &all), Ok(()));
        assert_eq!(validate_privileged_operation(connector, &all), invalid);
        let one = extended(json!({"requestedMessage": class, "connectorId": 1}));
        assert_eq!(validate_privileged_operation(connector, &one), Ok(()));
        assert_eq!(validate_privileged_operation(station, &one), invalid);
        let other = extended(json!({"requestedMessage": class, "connectorId": 2}));
        assert_eq!(validate_privileged_operation(connector, &other), invalid);
        for connector_id in [
            json!(0),
            json!(-1),
            json!(4_294_967_296_u64),
            json!(1.0),
            json!(null),
            json!("1"),
        ] {
            let operation =
                extended(json!({"requestedMessage": class, "connectorId": connector_id}));
            assert_eq!(
                validate_privileged_operation(station, &operation),
                invalid,
                "{class} {connector_id}"
            );
            assert_eq!(
                validate_privileged_operation(connector, &operation),
                invalid,
                "{class} {connector_id}"
            );
        }
    }

    let mut foreign = state.resources[0].resource.clone();
    foreign.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    assert_eq!(
        validate_privileged_operation(
            &foreign,
            &extended(json!({"requestedMessage":"MeterValues","connectorId":1}))
        ),
        invalid
    );
}

#[test]
fn schema_edition_and_certificate_requests_are_refused_by_code() {
    let state = enabled();
    let valid = extended(json!({"requestedMessage":"Heartbeat"}));
    let mut wrong = valid.clone();
    wrong.payload_schema =
        PayloadSchemaId::new("urn:OCPP:1.6:2019:12:TriggerMessageRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &wrong),
        Err(CommandErrorCode::InvalidParameters)
    );
    for payload in [
        json!([]),
        json!("Heartbeat"),
        json!({"requestedMessage":"Reset"}),
        json!({"requestedMessage":"Heartbeat","customData":{}}),
    ] {
        assert_eq!(
            validate_privileged_operation(&state.station, &extended(payload.clone())),
            Err(CommandErrorCode::InvalidParameters),
            "{payload}"
        );
    }
    let mut foreign = valid;
    foreign.protocol = ProtocolEdition::Ocpp201;
    assert_eq!(
        validate_privileged_operation(&state.station, &foreign),
        Err(CommandErrorCode::UnsupportedOperation)
    );
    // The schema accepts the certificate trigger; the bridge refuses it before any scope.
    for resource in [&state.station, &state.resources[0].resource] {
        assert_eq!(
            validate_privileged_operation(
                resource,
                &extended(json!({"requestedMessage":"SignChargePointCertificate"}))
            ),
            Err(CommandErrorCode::UnsupportedOperation)
        );
    }
}

#[test]
fn plain_trigger_message_still_refuses_the_whitepaper_classes() {
    let state = enabled();
    for class in [
        "LogStatusNotification",
        "SignedFirmwareStatusNotification",
        "SignChargePointCertificate",
    ] {
        let operation = trigger_operation(class, None);
        assert_eq!(
            validate_privileged_operation(&state.station, &operation),
            Err(CommandErrorCode::InvalidParameters),
            "{class}"
        );
    }
    let mut plain = snapshot(ProtocolEdition::Ocpp16j);
    advertise(&mut plain.capabilities.operations, "TriggerMessage");
    advertise(&mut plain.capabilities.operations, "GetLog");
    let [descriptor] = command_schemas(&plain)
        .into_iter()
        .filter(|descriptor| descriptor.action == "TriggerMessage")
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    assert!(!values(&descriptor).contains(&"LogStatusNotification".to_owned()));
}
