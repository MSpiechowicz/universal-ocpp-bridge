use super::*;
use uob_application::RuntimeResourceLimits;
use uob_contracts::{BridgeId, ProtocolActionName, ProtocolEdition, StationId};

fn key(request_id: i32) -> ReportKey {
    ReportKey {
        station: StationId::new("station").unwrap(),
        connection: CorrelationId::new("actual-connection").unwrap(),
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("NotifyReport").unwrap(),
        request_id,
        correlation: CorrelationId::new("bridge-correlation").unwrap(),
    }
}
fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
fn attempt(
    registry: &SharedReports,
    budget: &RuntimeResourceBudget,
    request: i32,
) -> Result<ReportRoute, SessionSubmitError> {
    let key = key(request);
    let admission = crate::multipart::reserve_report(
        key.clone(),
        crate::multipart::ReportLimits::default(),
        budget,
    )
    .unwrap();
    register(registry, key, resource(), admission).map(|(route, _)| route)
}

#[test]
fn used_native_ids_remain_retired_at_fixed_capacity_until_connection_teardown() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let registry = registry(&budget);
    drop(attempt(&registry, &budget, i32::MIN).unwrap());
    let bookkeeping = budget.snapshot().queued_payload_bytes;
    assert!(matches!(
        attempt(&registry, &budget, i32::MIN),
        Err(SessionSubmitError::Full)
    ));
    for request in 0..4095 {
        drop(attempt(&registry, &budget, request).unwrap());
    }
    assert!(matches!(
        attempt(&registry, &budget, i32::MAX),
        Err(SessionSubmitError::Full)
    ));
    assert_eq!(budget.snapshot().queued_payload_bytes, bookkeeping);
    disconnect(&registry);
    assert!(matches!(
        attempt(&registry, &budget, i32::MIN),
        Err(SessionSubmitError::Closed)
    ));
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    let replacement = super::registry(&budget);
    drop(attempt(&replacement, &budget, i32::MIN).unwrap());
    disconnect(&replacement);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}
