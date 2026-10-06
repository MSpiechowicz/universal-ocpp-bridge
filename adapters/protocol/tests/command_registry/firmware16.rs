use super::*;

fn operation(action: &str, schema: &str, payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(schema).unwrap(),
        payload,
    }
}

fn legacy(payload: Value) -> PrivilegedOcppOperation<Value> {
    operation(
        "UpdateFirmware",
        "urn:uob:ocpp16:UpdateFirmwareReference:1",
        payload,
    )
}

fn signed(payload: Value) -> PrivilegedOcppOperation<Value> {
    operation(
        "SignedUpdateFirmware",
        "urn:uob:ocpp16:SignedUpdateFirmwareReference:1",
        payload,
    )
}

#[test]
fn firmware_discovery_is_station_root_only_and_per_advertised_family() {
    let mut state = snapshot(ProtocolEdition::Ocpp16j);
    for action in ["UpdateFirmware", "SignedUpdateFirmware"] {
        state.resources[0]
            .capabilities
            .operations
            .push(SupportedOperation {
                operation: Operation::ProtocolAction {
                    protocol: ProtocolEdition::Ocpp16j,
                    action: action.to_owned(),
                },
                parameters: vec![],
            });
    }
    assert!(command_schemas(&state).is_empty());
    state.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "SignedUpdateFirmware".to_owned(),
        },
        parameters: vec![],
    });
    let [descriptor] = command_schemas(&state).try_into().unwrap();
    assert_eq!(descriptor.resource, state.station);
    assert_eq!(descriptor.action, "SignedUpdateFirmware");
    let fields = serde_json::to_value(&descriptor).unwrap()["fields"].clone();
    let names: Vec<_> = fields
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "artifactReference",
            "requestId",
            "retrieveDateTime",
            "installDateTime",
            "retries",
            "retryInterval"
        ]
    );
    assert!(!fields.to_string().contains("location"));
}

#[test]
fn firmware_requests_accept_only_reference_payloads_on_the_station_root() {
    let state = snapshot(ProtocolEdition::Ocpp16j);
    let valid = json!({"artifactReference":"fw.bin","retrieveDate":"2099-01-01T00:00:00Z"});
    assert_eq!(
        validate_privileged_operation(&state.station, &legacy(valid.clone())),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(
            &state.station,
            &signed(
                json!({"requestId":-1,"artifactReference":"fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z"})
            )
        ),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(&state.resources[0].resource, &legacy(valid.clone())),
        Err(CommandErrorCode::InvalidParameters)
    );
    for payload in [
        json!({"location":"http://x.invalid/fw.bin","retrieveDate":"2099-01-01T00:00:00Z"}),
        json!({"artifactReference":"fw.bin","retrieveDate":"2099-01-01T00:00:00Z","location":"http://x.invalid"}),
        json!({"artifactReference":"../fw.bin","retrieveDate":"2099-01-01T00:00:00Z"}),
        json!({"artifactReference":"fw.bin","retrieveDate":"tomorrow"}),
        json!({"artifactReference":"fw.bin","retrieveDate":"2099-01-01T00:00:00Z","retries":-1}),
    ] {
        assert_eq!(
            validate_privileged_operation(&state.station, &legacy(payload.clone())),
            Err(CommandErrorCode::InvalidParameters),
            "{payload}"
        );
    }
    // Like the published schema, an explicit null is an absent optional count.
    assert_eq!(
        validate_privileged_operation(
            &state.station,
            &legacy(
                json!({"artifactReference":"fw.bin","retrieveDate":"2099-01-01T00:00:00Z","retries":null})
            )
        ),
        Ok(())
    );
    let mut native = legacy(valid.clone());
    native.payload_schema =
        PayloadSchemaId::new("urn:OCPP:1.6:2019:12:UpdateFirmwareRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &native),
        Err(CommandErrorCode::InvalidParameters)
    );
    let mut foreign = legacy(valid);
    foreign.protocol = ProtocolEdition::Ocpp201;
    assert_eq!(
        validate_privileged_operation(&state.station, &foreign),
        Err(CommandErrorCode::UnsupportedOperation)
    );
}
