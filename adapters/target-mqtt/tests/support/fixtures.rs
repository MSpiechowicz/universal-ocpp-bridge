use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uob_application::{DeliveryId, TargetDelivery, TargetDeliveryClass, TargetMessage};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, CommandLifecycle, CommandResult,
    CommandReturnRoute, ContractVersion, Environment, EventEnvelope, EventId, EventOrigin,
    EventType, PrincipalId, ProcessInstanceId, ReleaseId, RequestId, ResourceCapabilities,
    ResourceRef, RuntimeIdentity, StationId, StationSnapshot, TargetInstanceId, TargetKind,
    TraceDirection, TraceId, TraceOutcome, TraceRecord, TraceSequence, TraceStage, TraceTarget,
    UtcTimestamp,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestEvent {
    pub watts: i32,
}

pub fn snapshot_delivery(
    bridge: &str,
    station: &str,
    delivery_id: &str,
) -> TargetDelivery<TestEvent> {
    let resource = resource(bridge, station);
    delivery(
        delivery_id,
        resource.clone(),
        TargetDeliveryClass::ReplaceableLatestState,
        TargetMessage::StationSnapshot(StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station: resource,
            observed_at: timestamp(),
            connectivity: uob_contracts::Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: vec![],
            transactions: vec![],
            current_values: vec![],
        }),
    )
}

pub fn event_delivery(
    bridge: &str,
    station: &str,
    delivery_id: &str,
    event_id: &str,
    environment: Environment,
) -> TargetDelivery<TestEvent> {
    let resource = resource(bridge, station);
    delivery(
        delivery_id,
        resource.clone(),
        TargetDeliveryClass::Durable,
        TargetMessage::DomainEvent(EventEnvelope {
            event_id: EventId::new(event_id).expect("event identity"),
            schema_version: ContractVersion::V1_INITIAL,
            runtime: RuntimeIdentity {
                environment,
                release_id: ReleaseId::new("test-release").expect("release identity"),
                release_digest: ArtifactDigest::new("sha256:test").expect("artifact digest"),
                process_instance_id: ProcessInstanceId::new("process-a").expect("process identity"),
            },
            resource,
            source_time: None,
            observed_at: timestamp(),
            event_type: EventType::new("meter.changed.v1").expect("event type"),
            origin: EventOrigin::Bridge,
            sequence: 1,
            correlation_id: None,
            causation_id: None,
            provenance: None,
            payload: TestEvent { watts: 7_200 },
        }),
    )
}

pub fn result_delivery(
    bridge: &str,
    station: &str,
    delivery_id: &str,
    request_id: &str,
) -> TargetDelivery<TestEvent> {
    let resource = resource(bridge, station);
    delivery(
        delivery_id,
        resource.clone(),
        TargetDeliveryClass::Durable,
        TargetMessage::CommandResult(CommandResult {
            schema_version: ContractVersion::V1_INITIAL,
            correlation_id: None,
            resource,
            return_route: CommandReturnRoute {
                request_id: RequestId::new(request_id).expect("request identity"),
                origin: AuthenticatedCommandOrigin::Management {
                    principal_id: PrincipalId::new("operator-a").expect("principal identity"),
                },
            },
            lifecycle: CommandLifecycle::Admitted,
            recorded_at: timestamp(),
            observed_effects: vec![],
        }),
    )
}

pub fn trace_delivery(
    bridge: &str,
    station: &str,
    delivery_id: &str,
    trace_id: &str,
) -> TargetDelivery<TestEvent> {
    let resource = resource(bridge, station);
    delivery(
        delivery_id,
        resource,
        TargetDeliveryClass::Durable,
        TargetMessage::Diagnostic(TraceRecord {
            schema_version: ContractVersion::V1_INITIAL,
            trace_id: TraceId::new(trace_id).expect("trace identity"),
            process_instance_id: ProcessInstanceId::new("process-a").expect("process identity"),
            trace_sequence: TraceSequence(1),
            target: Some(TraceTarget {
                instance_id: target_id(),
                kind: TargetKind::new("mqtt").expect("target kind"),
            }),
            correlation_id: None,
            parent_trace_id: None,
            stage: TraceStage::new("target.publish").expect("trace stage"),
            direction: TraceDirection::Outbound,
            observed_at: timestamp(),
            duration_micros: Some(12),
            outcome: TraceOutcome::Succeeded,
            redacted_details: None,
        }),
    )
}

pub fn resource(bridge: &str, station: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new(bridge).expect("bridge identity"),
        station_id: StationId::new(station).expect("station identity"),
        resource: None,
        native_protocol_reference: None,
    }
}

pub fn target_id() -> TargetInstanceId {
    TargetInstanceId::new("main").expect("target identity")
}

pub fn timestamp() -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH)
}

fn delivery(
    delivery_id: &str,
    resource: ResourceRef,
    class: TargetDeliveryClass,
    message: TargetMessage<TestEvent>,
) -> TargetDelivery<TestEvent> {
    TargetDelivery {
        delivery_id: DeliveryId::new(delivery_id).expect("delivery identity"),
        target_instance_id: target_id(),
        target_configuration_revision: 1,
        station_ordering_key: resource,
        deadline: timestamp(),
        class,
        message: Arc::new(message),
    }
}
