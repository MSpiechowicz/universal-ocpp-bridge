use std::{collections::BTreeMap, fmt::Write as _};

use serde::Serialize;
use uob_contracts::{BridgeId, Environment, StationSnapshot};

use crate::mapping::{MappingError, WirePublication, encode};

const DISCOVERY_PREFIX: &str = "homeassistant";
const MAX_TOPIC_BYTES: usize = 65_535;

pub(crate) fn publications(
    environment: Environment,
    bridge_id: &BridgeId,
    base: &str,
    snapshot: &StationSnapshot,
    maximum_payload_bytes: usize,
) -> Result<Vec<WirePublication>, MappingError> {
    if snapshot.station.bridge_id != *bridge_id {
        return Err(MappingError::IdentityMismatch);
    }

    let environment = environment_name(environment);
    let bridge_key = hex_identifier(bridge_id.as_str());
    let station_key = hex_identifier(snapshot.station.station_id.as_str());
    let node_id = format!("uob_{environment}_{bridge_key}");
    let device_id = format!("uob:{environment}:{bridge_key}:{station_key}");
    let state_topic = format!(
        "{base}/state/{}",
        super::mapping::encode_segment(snapshot.station.station_id.as_str())
    );
    let availability_topic = format!("{base}/availability");
    let device = Device {
        identifiers: vec![device_id],
        name: format!("UOB station {}", snapshot.station.station_id.as_str()),
        manufacturer: "Universal OCPP Bridge".to_owned(),
        model: "OCPP charging station".to_owned(),
    };

    let mut result = vec![publication(
        "binary_sensor",
        &node_id,
        &format!("station_{station_key}_connectivity"),
        &ConnectivityDiscovery {
            name: "Connectivity".to_owned(),
            unique_id: format!("uob:{environment}:{bridge_key}:{station_key}:connectivity"),
            state_topic: state_topic.clone(),
            value_template: "{{ value_json.connectivity.status }}".to_owned(),
            payload_on: "connected",
            payload_off: "disconnected",
            availability_topic: availability_topic.clone(),
            availability_template: "{{ value_json.status }}",
            payload_available: "online",
            payload_not_available: "offline",
            qos: 1,
            device: device.clone(),
        },
        maximum_payload_bytes,
    )?];

    let mut points = BTreeMap::new();
    for point in &snapshot.current_values {
        points.entry(point.point_id.as_str()).or_insert(point);
    }
    for resource in &snapshot.resources {
        for point in &resource.current_values {
            points.entry(point.point_id.as_str()).or_insert(point);
        }
    }
    for (point_id, point) in points {
        let point_key = hex_identifier(point_id);
        result.push(publication(
            "sensor",
            &node_id,
            &format!("station_{station_key}_point_{point_key}"),
            &SensorDiscovery {
                name: point_id.to_owned(),
                unique_id: format!(
                    "uob:{environment}:{bridge_key}:{station_key}:point:{point_key}"
                ),
                state_topic: state_topic.clone(),
                value_template: point_value_template(point_id),
                availability_topic: availability_topic.clone(),
                availability_template: "{{ value_json.status }}",
                payload_available: "online",
                payload_not_available: "offline",
                unit_of_measurement: point
                    .measurement
                    .as_ref()
                    .and_then(|measurement| measurement.original_unit.clone()),
                qos: 1,
                device: device.clone(),
            },
            maximum_payload_bytes,
        )?);
    }
    Ok(result)
}

fn publication(
    component: &str,
    node_id: &str,
    object_id: &str,
    payload: &impl Serialize,
    maximum_payload_bytes: usize,
) -> Result<WirePublication, MappingError> {
    let topic = format!("{DISCOVERY_PREFIX}/{component}/{node_id}/{object_id}/config");
    if topic.len() > MAX_TOPIC_BYTES {
        return Err(MappingError::TopicTooLong);
    }
    Ok(WirePublication {
        topic,
        retain: true,
        payload: encode(payload, maximum_payload_bytes)?,
    })
}

fn point_value_template(point_id: &str) -> String {
    let point_id = serde_json::to_string(point_id).expect("a string always serializes");
    format!(
        "{{% set ns = namespace(value = none) %}}{{% for point in value_json.current_values | default([]) %}}{{% if point.point_id == {point_id} and point.value is defined %}}{{% set ns.value = point.value.value %}}{{% endif %}}{{% endfor %}}{{% for resource in value_json.resources | default([]) %}}{{% for point in resource.current_values | default([]) %}}{{% if point.point_id == {point_id} and point.value is defined %}}{{% set ns.value = point.value.value %}}{{% endif %}}{{% endfor %}}{{% endfor %}}{{{{ ns.value }}}}"
    )
}

fn hex_identifier(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

const fn environment_name(environment: Environment) -> &'static str {
    match environment {
        Environment::Production => "production",
        Environment::Staging => "staging",
        Environment::Demo => "demo",
    }
}

#[derive(Clone, Serialize)]
struct Device {
    identifiers: Vec<String>,
    name: String,
    manufacturer: String,
    model: String,
}

#[derive(Serialize)]
struct ConnectivityDiscovery {
    name: String,
    unique_id: String,
    state_topic: String,
    value_template: String,
    payload_on: &'static str,
    payload_off: &'static str,
    availability_topic: String,
    availability_template: &'static str,
    payload_available: &'static str,
    payload_not_available: &'static str,
    qos: u8,
    device: Device,
}

#[derive(Serialize)]
struct SensorDiscovery {
    name: String,
    unique_id: String,
    state_topic: String,
    value_template: String,
    availability_topic: String,
    availability_template: &'static str,
    payload_available: &'static str,
    payload_not_available: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit_of_measurement: Option<String>,
    qos: u8,
    device: Device,
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use uob_contracts::{
        BridgeId, Connectivity, ContractVersion, Environment, ResourceCapabilities, ResourceRef,
        StationId, StationSnapshot, UtcTimestamp,
    };

    use super::publications;

    #[test]
    fn environment_and_encoded_identities_make_discovery_collision_free() {
        let bridge = BridgeId::new("a:b/c").expect("bridge identity");
        let snapshot = StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station: ResourceRef {
                bridge_id: bridge.clone(),
                station_id: StationId::new("x:y/z").expect("station identity"),
                resource: None,
                native_protocol_reference: None,
            },
            observed_at: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
            connectivity: Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: Vec::new(),
            transactions: Vec::new(),
            current_values: Vec::new(),
        };
        let mut topics = Vec::new();
        let mut unique_ids = Vec::new();
        for environment in [
            Environment::Production,
            Environment::Staging,
            Environment::Demo,
        ] {
            let publication = publications(
                environment,
                &bridge,
                "uob/v1/test/a%3Ab%2Fc",
                &snapshot,
                4096,
            )
            .expect("discovery mapping")
            .pop()
            .expect("connectivity discovery");
            topics.push(publication.topic);
            let payload: serde_json::Value =
                serde_json::from_slice(&publication.payload).expect("discovery JSON");
            unique_ids.push(payload["unique_id"].as_str().expect("unique ID").to_owned());
        }
        topics.sort();
        topics.dedup();
        unique_ids.sort();
        unique_ids.dedup();
        assert_eq!(topics.len(), 3);
        assert_eq!(unique_ids.len(), 3);
        assert!(
            topics
                .iter()
                .all(|topic| !topic.contains("a:b/c") && !topic.contains("x:y/z"))
        );
        assert!(
            unique_ids
                .iter()
                .all(|identity| !identity.contains("a:b/c"))
        );
    }
}
