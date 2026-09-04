use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uob_application::{DeliveryId, TargetDelivery, TargetDeliveryClass, TargetMessage};
use uob_contracts::{
    AccessMode, ArtifactDigest, AuthenticatedCommandOrigin, AvailabilityState, BridgeId,
    CanonicalConnectorId, CanonicalResource, ChargingResourceSnapshot, CommandLifecycle,
    CommandResult, CommandReturnRoute, ContractVersion, DataPointConstraints, DataPointDescriptor,
    DataPointValue, EngineeringUnit, Environment, EventEnvelope, EventId, EventOrigin, EventType,
    ExactDecimal, Freshness, MeasurementContext, MeasurementLocation, MeasurementMetadata,
    NativeProtocolReference, PointId, PrincipalId, ProcessInstanceId, Quality, QualityLevel,
    ReleaseId, RequestId, ResourceCapabilities, ResourceRef, RuntimeIdentity, SemanticName,
    StationId, StationSnapshot, TargetInstanceId, TargetKind, TraceDirection, TraceId,
    TraceOutcome, TraceRecord, TraceSequence, TraceStage, TraceTarget, TypedValue, UtcTimestamp,
    ValueType,
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

/// Snapshot carrying one connector resource with a descriptor and its latest measured value.
pub fn point_catalog_snapshot_delivery(
    bridge: &str,
    station: &str,
    delivery_id: &str,
) -> TargetDelivery<TestEvent> {
    let station_resource = resource(bridge, station);
    let connector = ResourceRef {
        resource: Some(CanonicalResource::Connector {
            connector_id: CanonicalConnectorId::new("1").expect("connector identity"),
        }),
        ..station_resource.clone()
    };
    delivery(
        delivery_id,
        station_resource.clone(),
        TargetDeliveryClass::ReplaceableLatestState,
        TargetMessage::StationSnapshot(StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station: station_resource,
            observed_at: timestamp(),
            connectivity: uob_contracts::Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: vec![ChargingResourceSnapshot {
                resource: connector.clone(),
                availability: AvailabilityState::Occupied,
                capabilities: ResourceCapabilities::default(),
                data_points: vec![point_descriptor(connector)],
                current_values: vec![point_value()],
            }],
            transactions: vec![],
            current_values: vec![],
        }),
    )
}

/// Stable identity of the point published by [`point_catalog_snapshot_delivery`].
pub fn catalog_point_id() -> PointId {
    PointId::new("ocpp16/connector-1/Energy.Active.Import.Register/all/Outlet")
        .expect("point identity")
}

pub fn point_descriptor(resource: ResourceRef) -> DataPointDescriptor {
    DataPointDescriptor {
        point_id: catalog_point_id(),
        resource,
        semantic_name: SemanticName::new("energy.active.import.register.v1")
            .expect("semantic name"),
        value_type: ValueType::Decimal,
        unit: Some(EngineeringUnit::WattHour),
        access: AccessMode::ReadOnly,
        constraints: DataPointConstraints::default(),
    }
}

pub fn point_value() -> DataPointValue {
    DataPointValue {
        point_id: catalog_point_id(),
        value: Some(TypedValue::Decimal(
            "1234.5".parse::<ExactDecimal>().expect("exact decimal"),
        )),
        source_time: Some(UtcTimestamp::new(
            OffsetDateTime::UNIX_EPOCH + std::time::Duration::from_secs(90),
        )),
        observed_at: timestamp(),
        quality: Quality {
            level: QualityLevel::Uncertain,
            reason: Some("signed_data_unverified".to_owned()),
        },
        freshness: Freshness::Fresh {
            valid_until: Some(UtcTimestamp::new(
                OffsetDateTime::UNIX_EPOCH + std::time::Duration::from_secs(600),
            )),
        },
        measurement: Some(MeasurementMetadata {
            original_value: "1234.500".to_owned(),
            original_unit: Some("Wh".to_owned()),
            measurand: Some(SemanticName::new("Energy.Active.Import.Register").expect("measurand")),
            phase: None,
            location: Some(MeasurementLocation::new("Outlet").expect("location")),
            context: Some(MeasurementContext::new("Sample.Periodic").expect("context")),
            protocol_reference: Some(NativeProtocolReference::Ocpp16 { connector_id: 1 }),
        }),
    }
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
