use std::collections::BTreeMap;

use uob_contracts::{BridgeId, DataPointDescriptor, DataPointValue, StationSnapshot};

use crate::mapping::{MappingError, WirePublication, encode, encode_segment};

const MAX_TOPIC_BYTES: usize = 65_535;

/// Builds the retained EMS/SCADA point catalog for one canonical station snapshot.
///
/// Descriptors and latest values are published as the canonical contract objects themselves, so a
/// broker-based EMS client reads exactly the documents the direct HTTP target serves. Units,
/// quality, freshness, and both timestamps therefore stay inside the canonical value and are never
/// re-derived by this adapter.
pub(crate) fn publications(
    bridge_id: &BridgeId,
    base: &str,
    snapshot: &StationSnapshot,
    maximum_payload_bytes: usize,
) -> Result<Vec<WirePublication>, MappingError> {
    if snapshot.station.bridge_id != *bridge_id {
        return Err(MappingError::IdentityMismatch);
    }
    let station = encode_segment(snapshot.station.station_id.as_str());

    let mut descriptors: BTreeMap<&str, &DataPointDescriptor> = BTreeMap::new();
    let mut values: BTreeMap<&str, &DataPointValue> = BTreeMap::new();
    for value in &snapshot.current_values {
        values.entry(value.point_id.as_str()).or_insert(value);
    }
    for resource in &snapshot.resources {
        if resource.resource.bridge_id != *bridge_id
            || resource.resource.station_id != snapshot.station.station_id
        {
            return Err(MappingError::IdentityMismatch);
        }
        for descriptor in &resource.data_points {
            if descriptor.resource != resource.resource {
                return Err(MappingError::IdentityMismatch);
            }
            descriptors
                .entry(descriptor.point_id.as_str())
                .or_insert(descriptor);
        }
        for value in &resource.current_values {
            values.entry(value.point_id.as_str()).or_insert(value);
        }
    }

    let mut result = Vec::with_capacity(descriptors.len().saturating_add(values.len()));
    for (point_id, descriptor) in descriptors {
        result.push(publication(
            base,
            "points",
            &station,
            point_id,
            descriptor,
            maximum_payload_bytes,
        )?);
    }
    for (point_id, value) in values {
        result.push(publication(
            base,
            "values",
            &station,
            point_id,
            value,
            maximum_payload_bytes,
        )?);
    }
    Ok(result)
}

fn publication(
    base: &str,
    category: &str,
    station: &str,
    point_id: &str,
    payload: &impl serde::Serialize,
    maximum_payload_bytes: usize,
) -> Result<WirePublication, MappingError> {
    let topic = format!("{base}/{category}/{station}/{}", encode_segment(point_id));
    if topic.len() > MAX_TOPIC_BYTES {
        return Err(MappingError::TopicTooLong);
    }
    Ok(WirePublication {
        topic,
        retain: true,
        payload: encode(payload, maximum_payload_bytes)?,
    })
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use uob_contracts::{
        AccessMode, AvailabilityState, BridgeId, CanonicalConnectorId, CanonicalResource,
        ChargingResourceSnapshot, Connectivity, ContractVersion, DataPointConstraints,
        DataPointDescriptor, DataPointValue, EngineeringUnit, ExactDecimal, Freshness, PointId,
        Quality, QualityLevel, ResourceCapabilities, ResourceRef, SemanticName, StationId,
        StationSnapshot, TypedValue, UtcTimestamp, ValueType,
    };

    use super::publications;

    fn station_ref(bridge: &BridgeId) -> ResourceRef {
        ResourceRef {
            bridge_id: bridge.clone(),
            station_id: StationId::new("station-7").expect("station identity"),
            resource: None,
            native_protocol_reference: None,
        }
    }

    fn connector_ref(bridge: &BridgeId) -> ResourceRef {
        ResourceRef {
            resource: Some(CanonicalResource::Connector {
                connector_id: CanonicalConnectorId::new("1").expect("connector identity"),
            }),
            ..station_ref(bridge)
        }
    }

    fn snapshot(bridge: &BridgeId) -> StationSnapshot {
        let connector = connector_ref(bridge);
        StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station: station_ref(bridge),
            observed_at: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
            connectivity: Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: vec![ChargingResourceSnapshot {
                resource: connector.clone(),
                availability: AvailabilityState::Available,
                capabilities: ResourceCapabilities::default(),
                data_points: vec![DataPointDescriptor {
                    point_id: PointId::new("ocpp16/connector-1/Energy/all/Outlet")
                        .expect("point identity"),
                    resource: connector,
                    semantic_name: SemanticName::new("energy.active.import.register.v1")
                        .expect("semantic name"),
                    value_type: ValueType::Decimal,
                    unit: Some(EngineeringUnit::WattHour),
                    access: AccessMode::ReadOnly,
                    constraints: DataPointConstraints::default(),
                }],
                current_values: vec![DataPointValue {
                    point_id: PointId::new("ocpp16/connector-1/Energy/all/Outlet")
                        .expect("point identity"),
                    value: Some(TypedValue::Decimal(
                        "1234.5".parse::<ExactDecimal>().expect("exact decimal"),
                    )),
                    source_time: Some(UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH)),
                    observed_at: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
                    quality: Quality {
                        level: QualityLevel::Good,
                        reason: None,
                    },
                    freshness: Freshness::Unknown,
                    measurement: None,
                }],
            }],
            transactions: Vec::new(),
            current_values: Vec::new(),
        }
    }

    #[test]
    fn descriptors_and_values_are_retained_under_separate_encoded_topics() {
        let bridge = BridgeId::new("site-01").expect("bridge identity");
        let publications = publications(&bridge, "uob/v1/demo/site-01", &snapshot(&bridge), 8192)
            .expect("point catalog");

        let topics = publications
            .iter()
            .map(|publication| publication.topic.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            topics,
            vec![
                "uob/v1/demo/site-01/points/station-7/ocpp16%2Fconnector-1%2FEnergy%2Fall%2FOutlet",
                "uob/v1/demo/site-01/values/station-7/ocpp16%2Fconnector-1%2FEnergy%2Fall%2FOutlet",
            ]
        );
        assert!(
            publications.iter().all(|publication| publication.retain),
            "an EMS client must read the current catalog from broker retention"
        );
    }

    #[test]
    fn a_foreign_resource_never_reaches_the_bridge_namespace() {
        let bridge = BridgeId::new("site-01").expect("bridge identity");
        let other = BridgeId::new("site-02").expect("bridge identity");
        let owned = snapshot(&bridge);
        let mut foreign_station = snapshot(&bridge);
        foreign_station.resources[0].resource.station_id =
            StationId::new("station-9").expect("station identity");

        assert!(publications(&bridge, "uob/v1/demo/site-01", &foreign_station, 8192).is_err());
        assert!(publications(&other, "uob/v1/demo/site-02", &owned, 8192).is_err());
    }
}
