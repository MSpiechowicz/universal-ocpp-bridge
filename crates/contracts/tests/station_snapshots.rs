use uob_contracts::{
    CanonicalResource, CapabilityError, Operation, ProtocolEdition, StationSnapshot, TypedValue,
};

const OCPP_16_SNAPSHOT: &str = include_str!("fixtures/station-snapshot-ocpp16-v1.json");
const OCPP_201_SNAPSHOT: &str = include_str!("fixtures/station-snapshot-ocpp201-v1.json");

#[test]
fn protocol_fixtures_retain_distinct_topologies_and_capability_details() {
    let ocpp16: StationSnapshot = serde_json::from_str(OCPP_16_SNAPSHOT).expect("OCPP 1.6 fixture");
    let ocpp201: StationSnapshot =
        serde_json::from_str(OCPP_201_SNAPSHOT).expect("OCPP 2.0.1 fixture");

    assert_eq!(ocpp16.resources.len(), 2);
    assert!(ocpp16.resources.iter().all(|resource| matches!(
        resource.resource.resource,
        Some(CanonicalResource::Connector { .. })
    )));
    assert_eq!(
        ocpp16.capabilities.protocol_details[0].protocol,
        ProtocolEdition::Ocpp16j
    );

    assert_eq!(ocpp201.resources.len(), 3);
    assert!(ocpp201.resources.iter().all(|resource| matches!(
        resource.resource.resource,
        Some(CanonicalResource::Evse { .. })
    )));
    assert_eq!(
        ocpp201.capabilities.protocol_details[0].protocol,
        ProtocolEdition::Ocpp201
    );
}

#[test]
fn operation_constraints_are_explicit_and_version_specific() {
    let ocpp16: StationSnapshot = serde_json::from_str(OCPP_16_SNAPSHOT).expect("OCPP 1.6 fixture");
    let ocpp201: StationSnapshot =
        serde_json::from_str(OCPP_201_SNAPSHOT).expect("OCPP 2.0.1 fixture");

    let limit16 = ocpp16.resources[0]
        .capabilities
        .require(&Operation::SetChargingLimit)
        .expect("connector limit support");
    assert_eq!(limit16.parameters.len(), 1);
    assert_eq!(limit16.parameters[0].name.as_str(), "current_amps");
    assert_eq!(
        limit16.parameters[0].constraints.maximum,
        Some(TypedValue::Decimal("32".parse().expect("exact decimal")))
    );

    let limit201 = ocpp201.resources[0]
        .capabilities
        .require(&Operation::SetChargingLimit)
        .expect("EVSE limit support");
    assert_eq!(limit201.parameters.len(), 2);
    assert_eq!(limit201.parameters[1].name.as_str(), "phases");
}

#[test]
fn omitted_operations_are_unsupported_instead_of_inferred() {
    let snapshot: StationSnapshot =
        serde_json::from_str(OCPP_16_SNAPSHOT).expect("OCPP 1.6 fixture");
    let read_only_connector = &snapshot.resources[1].capabilities;

    assert!(read_only_connector.operations.is_empty());
    assert!(!read_only_connector.supports(&Operation::Start));
    assert_eq!(
        read_only_connector.require(&Operation::Start),
        Err(CapabilityError::UnsupportedOperation(Operation::Start))
    );
}

#[test]
fn snapshots_round_trip_as_transport_neutral_observations() {
    for fixture in [OCPP_16_SNAPSHOT, OCPP_201_SNAPSHOT] {
        let snapshot: StationSnapshot = serde_json::from_str(fixture).expect("snapshot fixture");
        let serialized = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded: StationSnapshot =
            serde_json::from_str(&serialized).expect("deserialize snapshot");

        assert_eq!(decoded, snapshot);
        assert!(!serialized.contains("desired_state"));
        assert!(!serialized.contains("mqtt"));
        assert!(!serialized.contains("websocket_frame"));
    }
}
