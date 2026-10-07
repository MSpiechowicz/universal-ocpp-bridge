use super::*;

fn update(payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("UpdateFirmware").unwrap(),
        payload_schema: PayloadSchemaId::new("urn:uob:ocpp201:UpdateFirmwareReference:1").unwrap(),
        payload,
    }
}

fn advertise(capabilities: &mut uob_contracts::ResourceCapabilities) {
    capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: "UpdateFirmware".to_owned(),
        },
        parameters: vec![],
    });
}

#[test]
fn firmware201_discovery_is_station_root_only_and_never_offers_a_location() {
    let mut state = snapshot(ProtocolEdition::Ocpp201);
    advertise(&mut state.resources[0].capabilities);
    assert!(command_schemas(&state).is_empty());
    advertise(&mut state.capabilities);
    let [descriptor] = command_schemas(&state).try_into().unwrap();
    assert_eq!(descriptor.resource, state.station);
    assert_eq!(descriptor.protocol, ProtocolEdition::Ocpp201);
    assert_eq!(descriptor.action, "UpdateFirmware");
    assert_eq!(
        descriptor.payload_schema,
        "urn:uob:ocpp201:UpdateFirmwareReference:1"
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
            "requestId",
            "artifactReference",
            "retrieveDateTime",
            "installDateTime",
            "retries",
            "retryInterval"
        ]
    );
    for private in ["location", "signingCertificate", "signature"] {
        assert!(!fields.to_string().contains(private), "{private}");
    }
}

#[test]
fn firmware201_requests_accept_only_reference_payloads_on_the_station_root() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let valid = json!({"requestId":-7,"artifactReference":"fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z","installDateTime":"2099-01-02T00:00:00Z","retries":1,"retryInterval":30});
    assert_eq!(
        validate_privileged_operation(&state.station, &update(valid.clone())),
        Ok(())
    );
    let mut evse_root = state.station.clone();
    evse_root.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 0,
        connector_id: None,
    });
    assert_eq!(
        validate_privileged_operation(&evse_root, &update(valid.clone())),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(&state.resources[0].resource, &update(valid.clone())),
        Err(CommandErrorCode::InvalidParameters)
    );
    for payload in [
        json!({"artifactReference":"fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z"}),
        json!({"requestId":1,"location":"http://x.invalid/fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z"}),
        json!({"requestId":1,"artifactReference":"fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z","signature":"AA=="}),
        json!({"requestId":1,"artifactReference":"../fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z"}),
        json!({"requestId":1,"artifactReference":"fw.bin"}),
        json!({"requestId":1,"artifactReference":"fw.bin","retrieveDateTime":"2099-01-02T00:00:00Z","installDateTime":"2099-01-01T00:00:00Z"}),
        json!({"requestId":2_147_483_648_i64,"artifactReference":"fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z"}),
        json!({"requestId":1,"artifactReference":"fw.bin","retrieveDateTime":"2099-01-01T00:00:00Z","retries":-1}),
    ] {
        assert_eq!(
            validate_privileged_operation(&state.station, &update(payload.clone())),
            Err(CommandErrorCode::InvalidParameters),
            "{payload}"
        );
    }
    let mut native = update(valid.clone());
    native.payload_schema =
        PayloadSchemaId::new("urn:OCPP:Cp:2:2020:3:UpdateFirmwareRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &native),
        Err(CommandErrorCode::InvalidParameters)
    );
    let mut whitepaper = update(valid);
    whitepaper.action = ProtocolActionName::new("SignedUpdateFirmware").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &whitepaper),
        Err(CommandErrorCode::UnsupportedOperation)
    );
}
