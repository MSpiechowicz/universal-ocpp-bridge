use super::*;

fn operation(payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("GetCompositeSchedule").unwrap(),
        payload_schema: PayloadSchemaId::new("urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest")
            .unwrap(),
        payload,
    }
}

#[test]
fn schedule_discovery_requires_explicit_exact_resource_and_edition() {
    let mut state = snapshot(ProtocolEdition::Ocpp16j);
    assert!(command_schemas(&state).is_empty());
    let capability = SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "GetCompositeSchedule".to_owned(),
        },
        parameters: vec![],
    };
    state.capabilities.operations.push(capability.clone());
    state.resources[0].capabilities.operations.push(capability);
    let schemas = command_schemas(&state);
    assert_eq!(
        schemas
            .iter()
            .map(|schema| &schema.resource)
            .collect::<Vec<_>>(),
        vec![&state.station, &state.resources[0].resource]
    );
    state.resources[0].resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 {
        connector_id: u32::try_from(i32::MAX).unwrap() + 1,
    });
    assert_eq!(
        command_schemas(&state)
            .iter()
            .map(|schema| &schema.resource)
            .collect::<Vec<_>>(),
        vec![&state.station]
    );
    state.connectivity = Connectivity::Connected {
        protocol: ProtocolEdition::Ocpp201,
        connected_at: state.observed_at,
        last_message_at: None,
    };
    assert!(command_schemas(&state).is_empty());
}

#[test]
fn schedule_parameters_never_widen_scope_or_accept_null_unknown_or_invalid_ranges() {
    let state = snapshot(ProtocolEdition::Ocpp16j);
    for payload in [
        json!({"connectorId":0,"duration":1}),
        json!({"connectorId":0,"duration":2_147_483_647,"chargingRateUnit":"A"}),
    ] {
        assert_eq!(
            validate_privileged_operation(&state.station, &operation(payload)),
            Ok(())
        );
    }
    assert_eq!(
        validate_privileged_operation(
            &state.resources[0].resource,
            &operation(json!({"connectorId":1,"duration":60,"chargingRateUnit":"W"}))
        ),
        Ok(())
    );
    for payload in [
        json!({}),
        json!({"connectorId":0}),
        json!({"connectorId":0,"duration":null}),
        json!({"connectorId":0,"duration":0}),
        json!({"connectorId":0,"duration":-1}),
        json!({"connectorId":0,"duration":1.5}),
        json!({"connectorId":0,"duration":2_147_483_648_u64}),
        json!({"connectorId":1,"duration":60}),
        json!({"connectorId":-1,"duration":60}),
        json!({"connectorId":0,"duration":60,"chargingRateUnit":null}),
        json!({"connectorId":0,"duration":60,"chargingRateUnit":"kW"}),
        json!({"connectorId":0,"duration":60,"other":1}),
    ] {
        assert_eq!(
            validate_privileged_operation(&state.station, &operation(payload)),
            Err(CommandErrorCode::InvalidParameters)
        );
    }
    assert_eq!(
        validate_privileged_operation(
            &state.resources[0].resource,
            &operation(json!({"connectorId":0,"duration":60}))
        ),
        Err(CommandErrorCode::InvalidParameters)
    );
    let mut request = operation(json!({"connectorId":0,"duration":60}));
    request.protocol = ProtocolEdition::Ocpp201;
    assert_eq!(
        validate_privileged_operation(&state.station, &request),
        Err(CommandErrorCode::InvalidParameters)
    );
    request.protocol = ProtocolEdition::Ocpp16j;
    request.payload_schema = PayloadSchemaId::new("foreign").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &request),
        Err(CommandErrorCode::InvalidParameters)
    );
}
