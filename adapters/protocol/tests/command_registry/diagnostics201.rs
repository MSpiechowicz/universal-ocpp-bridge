use super::*;

fn log(payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("GetLog").unwrap(),
        payload_schema: PayloadSchemaId::new("urn:uob:ocpp201:GetLogReference:1").unwrap(),
        payload,
    }
}

fn advertise(capabilities: &mut uob_contracts::ResourceCapabilities) {
    capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: "GetLog".to_owned(),
        },
        parameters: vec![],
    });
}

#[test]
fn log201_discovery_is_station_root_only_and_never_offers_a_location() {
    let mut state = snapshot(ProtocolEdition::Ocpp201);
    advertise(&mut state.resources[0].capabilities);
    assert!(command_schemas(&state).is_empty());
    advertise(&mut state.capabilities);
    let [descriptor] = command_schemas(&state).try_into().unwrap();
    assert_eq!(descriptor.resource, state.station);
    assert_eq!(descriptor.protocol, ProtocolEdition::Ocpp201);
    assert_eq!(descriptor.action, "GetLog");
    assert_eq!(
        descriptor.payload_schema,
        "urn:uob:ocpp201:GetLogReference:1"
    );
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
            "logType",
            "requestId",
            "oldestTimestamp",
            "latestTimestamp",
            "retries",
            "retryInterval"
        ]
    );
    assert!(!names.iter().any(|name| name.contains("ocation")));
    assert_eq!(
        fields[0]["enum_values"],
        json!(["DiagnosticsLog", "SecurityLog"])
    );
}

#[test]
fn log201_requests_accept_only_reference_payloads_on_the_station_root() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let valid = json!({"logType":"DiagnosticsLog","requestId":-7,"oldestTimestamp":"2099-01-01T00:00:00Z","latestTimestamp":"2099-01-02T00:00:00Z","retries":1,"retryInterval":30});
    assert_eq!(
        validate_privileged_operation(&state.station, &log(valid.clone())),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(
            &state.station,
            &log(json!({"logType":"SecurityLog","requestId":0}))
        ),
        Ok(())
    );
    let mut evse_root = state.station.clone();
    evse_root.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 0,
        connector_id: None,
    });
    assert_eq!(
        validate_privileged_operation(&evse_root, &log(valid.clone())),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(&state.resources[0].resource, &log(valid.clone())),
        Err(CommandErrorCode::InvalidParameters)
    );
    for payload in [
        json!({"requestId":1}),
        json!({"logType":"SecurityLog"}),
        json!({"logType":"FirmwareLog","requestId":1}),
        json!({"logType":"SecurityLog","requestId":1,"log":{"remoteLocation":"https://x.invalid/"}}),
        json!({"logType":"SecurityLog","requestId":1,"remoteLocation":"https://x.invalid/"}),
        json!({"logType":"SecurityLog","requestId":2_147_483_648_i64}),
        json!({"logType":"SecurityLog","requestId":1,"retries":-1}),
        json!({"logType":"SecurityLog","requestId":1,"oldestTimestamp":"2099-01-02T00:00:00Z","latestTimestamp":"2099-01-01T00:00:00Z"}),
    ] {
        assert_eq!(
            validate_privileged_operation(&state.station, &log(payload.clone())),
            Err(CommandErrorCode::InvalidParameters),
            "{payload}"
        );
    }
    let mut native = log(valid.clone());
    native.payload_schema = PayloadSchemaId::new("urn:OCPP:Cp:2:2020:3:GetLogRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &native),
        Err(CommandErrorCode::InvalidParameters)
    );
    // The 1.6 Whitepaper schema is not a 2.0.1 schema, and the 1.6 reference rejects 2.0.1.
    let mut whitepaper = log(valid);
    whitepaper.payload_schema = PayloadSchemaId::new("urn:uob:ocpp16:GetLogReference:1").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &whitepaper),
        Err(CommandErrorCode::InvalidParameters)
    );
    let mut diagnostics = log(json!({}));
    diagnostics.action = ProtocolActionName::new("GetDiagnostics").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &diagnostics),
        Err(CommandErrorCode::UnsupportedOperation)
    );
}
