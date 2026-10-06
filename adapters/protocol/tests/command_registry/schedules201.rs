use super::*;
use uob_contracts::{CanonicalResource, ResourceRef};

fn operation(action: &str, payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(format!("urn:OCPP:Cp:2:2020:3:{action}Request"))
            .unwrap(),
        payload,
    }
}

/// The fixture's first resource with its connector removed: native EVSE 1 itself.
fn evse(state: &StationSnapshot) -> ResourceRef {
    let mut resource = state.resources[0].resource.clone();
    let Some(CanonicalResource::Evse { connector_id, .. }) = &mut resource.resource else {
        panic!("EVSE fixture")
    };
    *connector_id = None;
    resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    resource
}

#[test]
fn k08_and_k09_discovery_requires_exact_advertised_station_or_evse_scope() {
    let mut state = snapshot(ProtocolEdition::Ocpp201);
    let mut evse_entry = state.resources[0].clone();
    evse_entry.resource = evse(&state);
    state.resources.insert(0, evse_entry);
    for action in ["GetCompositeSchedule", "GetChargingProfiles"] {
        let capability = SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: action.to_owned(),
            },
            parameters: vec![],
        };
        state.capabilities.operations.push(capability.clone());
        // A connector capability (resource 1) alone never yields a descriptor.
        for entry in &mut state.resources[..2] {
            entry.capabilities.operations.push(capability.clone());
        }
    }
    let schemas = command_schemas(&state);
    let discovered = schemas
        .iter()
        .map(|schema| (schema.action, &schema.resource))
        .collect::<Vec<_>>();
    assert_eq!(
        discovered,
        [
            ("GetCompositeSchedule", &state.station),
            ("GetCompositeSchedule", &state.resources[0].resource),
            ("GetChargingProfiles", &state.station),
            ("GetChargingProfiles", &state.resources[0].resource),
        ]
    );
    let station_k09 = &schemas[2];
    assert_eq!(
        station_k09.payload_schema,
        "urn:OCPP:Cp:2:2020:3:GetChargingProfilesRequest"
    );
    // Station scope may omit evseId (all EVSEs); EVSE scope must name itself.
    assert!(
        !station_k09
            .fields
            .iter()
            .any(|f| f.name == "evseId" && f.required)
    );
    assert!(
        schemas[3]
            .fields
            .iter()
            .any(|f| f.name == "evseId" && f.required)
    );
}

#[test]
fn k08_and_k09_requests_validate_scope_criteria_and_pinned_schemas() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let evse = evse(&state);
    let composite = |payload| operation("GetCompositeSchedule", payload);
    let reports = |payload| operation("GetChargingProfiles", payload);
    for (resource, operation) in [
        (&state.station, composite(json!({"evseId":0,"duration":1}))),
        (
            &evse,
            composite(json!({"evseId":1,"duration":i32::MAX,"chargingRateUnit":"A"})),
        ),
        (
            &state.station,
            reports(json!({"requestId":i32::MIN,"chargingProfile":{"chargingProfileId":[1]}})),
        ),
        (
            &state.station,
            reports(json!({"requestId":0,"evseId":0,"chargingProfile":{"stackLevel":0}})),
        ),
        (
            &state.station,
            reports(
                json!({"requestId":0,"evseId":2,"chargingProfile":{"chargingLimitSource":["EMS","SO","Other","CSO"]}}),
            ),
        ),
        (
            &evse,
            reports(
                json!({"requestId":i32::MAX,"evseId":1,"chargingProfile":{"chargingProfilePurpose":"TxProfile"}}),
            ),
        ),
    ] {
        assert_eq!(validate_privileged_operation(resource, &operation), Ok(()));
    }
    for (resource, operation) in [
        (&state.station, composite(json!({"evseId":1,"duration":1}))),
        (&evse, composite(json!({"evseId":0,"duration":1}))),
        (
            &state.resources[0].resource,
            composite(json!({"evseId":1,"duration":1})),
        ),
        (&state.station, composite(json!({"evseId":0,"duration":0}))),
        (
            &state.station,
            composite(json!({"evseId":0,"duration":1,"chargingRateUnit":"kW"})),
        ),
        (
            &state.station,
            composite(json!({"evseId":0,"duration":1,"customData":{"vendorId":"v"}})),
        ),
        (
            &state.station,
            reports(json!({"requestId":1,"chargingProfile":{}})),
        ),
        (
            &state.station,
            reports(
                json!({"requestId":1,"chargingProfile":{"chargingProfileId":[1],"stackLevel":0}}),
            ),
        ),
        (
            &state.station,
            reports(json!({"requestId":1,"evseId":-1,"chargingProfile":{"stackLevel":0}})),
        ),
        (
            &evse,
            reports(json!({"requestId":1,"chargingProfile":{"stackLevel":0}})),
        ),
        (
            &evse,
            reports(json!({"requestId":1,"evseId":2,"chargingProfile":{"stackLevel":0}})),
        ),
        (
            &state.station,
            reports(json!({"requestId":1,"chargingProfile":{"stackLevel":0},"extra":1})),
        ),
    ] {
        assert_eq!(
            validate_privileged_operation(resource, &operation),
            Err(CommandErrorCode::InvalidParameters),
            "{:?}",
            operation.payload
        );
    }
    // The 1.6 schema remains the only one accepted for the 1.6 edition, and vice versa.
    let mut wrong = composite(json!({"evseId":0,"duration":1}));
    wrong.payload_schema =
        PayloadSchemaId::new("urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &wrong),
        Err(CommandErrorCode::InvalidParameters)
    );
}
