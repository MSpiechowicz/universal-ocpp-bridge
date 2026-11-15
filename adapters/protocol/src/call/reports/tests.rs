use super::*;
use crate::call::types::SessionSubmitError;
use uob_application::RuntimeResourceLimits;
use uob_contracts::CorrelationId;
use uob_contracts::{BridgeId, MonitoringQuery201, ProtocolActionName, ProtocolEdition, StationId};

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
    register(registry, key, resource(), RouteContext::Device, admission).map(|(route, _)| route)
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

fn monitoring_key(request_id: i32) -> ReportKey {
    ReportKey {
        action: ProtocolActionName::new("NotifyMonitoringReport").unwrap(),
        ..key(request_id)
    }
}

fn monitoring_context(request_id: i32) -> RouteContext {
    RouteContext::Monitoring(MonitoringQuery201::GetMonitoringReport {
        request_id,
        criteria: vec![],
        component_variables: vec![],
    })
}

fn monitoring_fragment(request_id: i32, sequence: u32, tbc: bool) -> serde_json::Value {
    serde_json::json!({
        "requestId": request_id, "seqNo": sequence, "tbc": tbc,
        "generatedAt": "2026-11-10T09:15:00Z",
        "monitor": [{
            "component": {"name": "EVSE", "evse": {"id": 1}}, "variable": {"name": "Power"},
            "variableMonitoring": [{"id": 4003, "transaction": true, "value": 2.5, "type": "Delta", "severity": 6}],
        }],
    })
}

fn monitoring_route(
    registry: &SharedReports,
    budget: &RuntimeResourceBudget,
    request_id: i32,
) -> ReportRoute {
    let key = monitoring_key(request_id);
    let admission = crate::multipart::reserve_report(
        key.clone(),
        crate::multipart::ReportLimits::default(),
        budget,
    )
    .unwrap();

    register(
        registry,
        key,
        resource(),
        monitoring_context(request_id),
        admission,
    )
    .map(|(route, _)| route)
    .unwrap()
}

#[test]
fn monitoring_fragments_are_default_off_and_enabled_independently() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let registry = registry(&budget);
    let payload = monitoring_fragment(7, 0, false);
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &payload, 512, &budget),
        None
    );

    // Enabling another report namespace does not open this one.
    enable(&registry, ReportKind::Device);
    enable(&registry, ReportKind::ChargingProfiles);
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &payload, 512, &budget),
        None
    );

    // Once enabled, a well-formed unsolicited fragment is acknowledged and a malformed one is not.
    enable(&registry, ReportKind::Monitoring);
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &payload, 512, &budget),
        Some(true)
    );
    let mut malformed = payload;
    malformed.as_object_mut().unwrap().remove("seqNo");
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &malformed, 512, &budget),
        Some(false)
    );

    // Teardown closes every namespace again.
    disconnect(&registry);
    assert_eq!(
        route(
            &registry,
            ReportKind::Monitoring,
            &monitoring_fragment(7, 0, false),
            512,
            &budget
        ),
        None
    );
}

#[tokio::test]
async fn a_monitoring_route_carries_the_native_sequence_and_sanitized_monitors() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let registry = registry(&budget);
    let mut collected = monitoring_route(&registry, &budget, 70);
    // Out of order on purpose: ordering is the collector's judgement, from the native seqNo.
    for (sequence, tbc) in [(0, true), (2, false)] {
        let payload = monitoring_fragment(70, sequence, tbc);
        assert_eq!(
            route(&registry, ReportKind::Monitoring, &payload, 512, &budget),
            Some(true)
        );
    }
    // Another request ID is not this route's fragment.
    let other = monitoring_fragment(71, 1, false);
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &other, 512, &budget),
        Some(true)
    );

    let mut seen = Vec::new();
    for _ in 0..2 {
        let ingress = collected.next().await.unwrap().unwrap();
        let FragmentMetadata::Monitoring(metadata) = ingress.metadata else {
            panic!("monitoring metadata");
        };
        assert_eq!(
            (ingress.fragment.sequence, ingress.fragment.more),
            (metadata.sequence, metadata.more)
        );
        assert_eq!(ingress.fragment.items.len(), 1);
        seen.push((metadata.sequence, metadata.more, metadata.monitors));
    }
    assert_eq!(seen, [(0, true, 1), (2, false, 1)]);
}

#[tokio::test]
async fn an_invalid_monitoring_fragment_fails_its_own_route_only() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let registry = registry(&budget);
    let mut broken = monitoring_route(&registry, &budget, 80);
    let mut healthy = monitoring_route(&registry, &budget, 81);

    let mut severity = monitoring_fragment(80, 0, true);
    severity["monitor"][0]["variableMonitoring"][0]["severity"] = serde_json::json!(10);
    // Well formed for the schema but not representable: refused to the charger.
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &severity, 512, &budget),
        Some(false)
    );
    assert!(matches!(
        broken.next().await,
        Err(ReportFailure::InvalidFragment)
    ));

    let payload = monitoring_fragment(81, 0, false);
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &payload, 512, &budget),
        Some(true)
    );
    assert!(healthy.next().await.unwrap().is_some());

    // Schema violations for a live route fail it and are refused as well.
    let mut extra = monitoring_fragment(81, 1, false);
    extra["unexpected"] = serde_json::json!(true);
    assert_eq!(
        route(&registry, ReportKind::Monitoring, &extra, 512, &budget),
        Some(false)
    );
    assert!(matches!(
        healthy.next().await,
        Err(ReportFailure::InvalidFragment)
    ));
}

#[test]
fn a_monitoring_route_cannot_open_under_another_native_action() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let registry = registry(&budget);
    let admission = |key: &ReportKey| {
        crate::multipart::reserve_report(
            key.clone(),
            crate::multipart::ReportLimits::default(),
            &budget,
        )
        .unwrap()
    };
    // The route context and the key's action name the same native namespace, or nothing opens.
    let wrong = key(9);
    assert!(matches!(
        register(
            &registry,
            wrong.clone(),
            resource(),
            monitoring_context(9),
            admission(&wrong)
        ),
        Err(SessionSubmitError::InvalidRequest)
    ));
    let device = monitoring_key(9);
    assert!(matches!(
        register(
            &registry,
            device.clone(),
            resource(),
            RouteContext::Device,
            admission(&device)
        ),
        Err(SessionSubmitError::InvalidRequest)
    ));
}
