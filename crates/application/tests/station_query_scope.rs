use uob_application::{TargetQueryAuthorization, TargetQueryPermission, TargetResourceScope};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalResource, NativeProtocolReference, ResourceRef,
    StationId, TargetInstanceId,
};

#[test]
fn child_grants_do_not_authorize_whole_station_inventory() {
    let station = ResourceRef {
        bridge_id: BridgeId::new("bridge-a").unwrap(),
        station_id: StationId::new("station-a").unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let mut child = station.clone();
    child.resource = Some(CanonicalResource::Connector {
        connector_id: CanonicalConnectorId::new("connector-1").unwrap(),
    });
    child.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 { connector_id: 1 });
    let granted_child = TargetQueryAuthorization::new(
        TargetInstanceId::new("target-a").unwrap(),
        vec![TargetQueryPermission::StationSnapshots],
        vec![TargetResourceScope::Resource(child)],
    );
    assert!(granted_child.station_resources().next().is_none());
    let mut controller = station.clone();
    controller.native_protocol_reference =
        Some(NativeProtocolReference::Ocpp16 { connector_id: 0 });
    let granted_station = TargetQueryAuthorization::new(
        TargetInstanceId::new("target-a").unwrap(),
        vec![TargetQueryPermission::StationSnapshots],
        vec![TargetResourceScope::Resource(controller)],
    );
    assert_eq!(
        granted_station.station_resources().collect::<Vec<_>>(),
        vec![station]
    );
}
