//! Canonical records for both OCPP resource models.

use uob_contracts::{
    AccessMode, AvailabilityState, BridgeId, CanonicalConnectorId, CanonicalEvseId,
    CanonicalResource, ChargingResourceSnapshot, Connectivity, ContractVersion,
    DataPointConstraints, DataPointDescriptor, DataPointValue, EngineeringUnit, ExactDecimal,
    Freshness, MeasurementContext, MeasurementMetadata, MeasurementPhase, NativeProtocolReference,
    PointId, Quality, QualityLevel, ResourceCapabilities, ResourceRef, SemanticName, StationId,
    StationSnapshot, TypedValue, UtcTimestamp, ValueType,
};

pub(crate) fn bridge() -> BridgeId {
    BridgeId::new("site-01").expect("bridge identity")
}

pub(crate) fn station_reference(station_id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: bridge(),
        station_id: StationId::new(station_id).expect("station identity"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp(seconds: u64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
}

/// OCPP 1.6 station: one connector resource plus a station-level connector-zero meter.
pub(crate) fn ocpp16_station() -> StationSnapshot {
    let station = station_reference("station-a");
    let connector = ResourceRef {
        resource: Some(CanonicalResource::Connector {
            connector_id: CanonicalConnectorId::new("1").expect("connector identity"),
        }),
        native_protocol_reference: Some(NativeProtocolReference::Ocpp16 { connector_id: 1 }),
        ..station.clone()
    };
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: station.clone(),
        observed_at: timestamp(30),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![ChargingResourceSnapshot {
            resource: connector.clone(),
            availability: AvailabilityState::Occupied,
            capabilities: ResourceCapabilities::default(),
            data_points: vec![energy_descriptor(connector.clone())],
            current_values: vec![energy_value()],
        }],
        transactions: vec![],
        // A connector-zero meter is a station-level value and carries no snapshot descriptor.
        current_values: vec![station_meter_value()],
    }
}

/// OCPP 2.0.1 station: one EVSE-scoped connector resource.
pub(crate) fn ocpp201_station() -> StationSnapshot {
    let station = station_reference("station-b");
    let evse = ResourceRef {
        resource: Some(CanonicalResource::Evse {
            evse_id: CanonicalEvseId::new("1").expect("evse identity"),
            connector_id: Some(CanonicalConnectorId::new("1").expect("connector identity")),
        }),
        native_protocol_reference: Some(NativeProtocolReference::Ocpp201 {
            evse_id: 1,
            connector_id: Some(1),
        }),
        ..station.clone()
    };
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station,
        observed_at: timestamp(31),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![ChargingResourceSnapshot {
            resource: evse.clone(),
            availability: AvailabilityState::Available,
            capabilities: ResourceCapabilities::default(),
            data_points: vec![energy_descriptor(evse)],
            current_values: vec![energy_value()],
        }],
        transactions: vec![],
        current_values: vec![],
    }
}

/// A station inside the configured target's scope but outside every test credential's scope.
pub(crate) fn unscoped_station() -> StationSnapshot {
    let station = station_reference("station-unscoped");
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station,
        observed_at: timestamp(32),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        transactions: vec![],
        current_values: vec![station_meter_value()],
    }
}

pub(crate) fn energy_point_id() -> PointId {
    PointId::new("energy.active.import.register").expect("point identity")
}

pub(crate) fn station_meter_point_id() -> PointId {
    PointId::new("station.meter.active.import").expect("point identity")
}

pub(crate) fn energy_descriptor(resource: ResourceRef) -> DataPointDescriptor {
    DataPointDescriptor {
        point_id: energy_point_id(),
        resource,
        semantic_name: SemanticName::new("energy.active.import.register.v1").expect("semantic"),
        value_type: ValueType::Decimal,
        unit: Some(EngineeringUnit::WattHour),
        access: AccessMode::ReadOnly,
        constraints: DataPointConstraints::default(),
    }
}

/// A value whose exact decimal must survive JSON rendering digit for digit.
pub(crate) fn energy_value() -> DataPointValue {
    DataPointValue {
        point_id: energy_point_id(),
        value: Some(TypedValue::Decimal(ExactDecimal::new(123_456_789, 3))),
        source_time: Some(timestamp(29)),
        observed_at: timestamp(30),
        quality: Quality {
            level: QualityLevel::Good,
            reason: None,
        },
        freshness: Freshness::Fresh { valid_until: None },
        measurement: Some(MeasurementMetadata {
            original_value: "123456.789".to_owned(),
            original_unit: Some("Wh".to_owned()),
            measurand: Some(SemanticName::new("Energy.Active.Import.Register").expect("measurand")),
            phase: Some(MeasurementPhase::new("L1-N").expect("phase")),
            context: Some(MeasurementContext::new("Sample.Periodic").expect("context")),
            location: None,
            protocol_reference: Some(NativeProtocolReference::Ocpp16 { connector_id: 1 }),
        }),
    }
}

/// A station-level value with no source timestamp and uncertain quality.
pub(crate) fn station_meter_value() -> DataPointValue {
    DataPointValue {
        point_id: station_meter_point_id(),
        value: None,
        source_time: None,
        observed_at: timestamp(30),
        quality: Quality {
            level: QualityLevel::Uncertain,
            reason: Some("meter.not_reported".to_owned()),
        },
        freshness: Freshness::Unknown,
        measurement: None,
    }
}
