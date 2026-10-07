use super::*;

fn operation(action: &str, schema: &str, payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(schema).unwrap(),
        payload,
    }
}

fn diagnostics(payload: Value) -> PrivilegedOcppOperation<Value> {
    operation(
        "GetDiagnostics",
        "urn:uob:ocpp16:GetDiagnosticsReference:1",
        payload,
    )
}

fn log(payload: Value) -> PrivilegedOcppOperation<Value> {
    operation("GetLog", "urn:uob:ocpp16:GetLogReference:1", payload)
}

#[test]
fn log_discovery_is_station_root_only_and_per_advertised_family() {
    let mut state = snapshot(ProtocolEdition::Ocpp16j);
    for action in ["GetDiagnostics", "GetLog"] {
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
            action: "GetLog".to_owned(),
        },
        parameters: vec![],
    });
    let [descriptor] = command_schemas(&state).try_into().unwrap();
    assert_eq!(descriptor.resource, state.station);
    assert_eq!(descriptor.action, "GetLog");
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
fn log_requests_validate_exact_reference_schemas_and_never_accept_locations() {
    let station = snapshot(ProtocolEdition::Ocpp16j).station;
    assert!(validate_privileged_operation(&station, &diagnostics(json!({}))).is_ok());
    assert!(
        validate_privileged_operation(
            &station,
            &log(json!({"logType":"SecurityLog","requestId":-5}))
        )
        .is_ok()
    );
    for (operation, code) in [
        (
            diagnostics(json!({"location":"ftp://example.invalid/"})),
            CommandErrorCode::InvalidParameters,
        ),
        (
            log(json!({"logType":"SecurityLog","requestId":1,"log":{"remoteLocation":"x"}})),
            CommandErrorCode::InvalidParameters,
        ),
        (
            log(json!({"logType":"FirmwareLog","requestId":1})),
            CommandErrorCode::InvalidParameters,
        ),
        (
            operation(
                "GetDiagnostics",
                "urn:OCPP:1.6:2019:12:GetDiagnosticsRequest",
                json!({"location":"ftp://example.invalid/"}),
            ),
            CommandErrorCode::InvalidParameters,
        ),
    ] {
        assert_eq!(
            validate_privileged_operation(&station, &operation),
            Err(code),
            "{:?}",
            operation.payload
        );
    }
    let connector = snapshot(ProtocolEdition::Ocpp16j).resources[0]
        .resource
        .clone();
    assert_eq!(
        validate_privileged_operation(&connector, &diagnostics(json!({}))),
        Err(CommandErrorCode::InvalidParameters)
    );
    let mut legacy_on_201 = diagnostics(json!({}));
    legacy_on_201.protocol = ProtocolEdition::Ocpp201;
    assert_eq!(
        validate_privileged_operation(&station, &legacy_on_201),
        Err(CommandErrorCode::UnsupportedOperation)
    );
}
