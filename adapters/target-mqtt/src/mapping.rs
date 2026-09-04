use std::io;

use serde::Serialize;
use uob_application::{ConfigurationError, ConfigurationErrorCode, TargetDelivery, TargetMessage};
use uob_contracts::{
    BridgeId, CommandResult, ContractVersion, Environment, RequestId, ResourceRef, StationSnapshot,
    TargetInstanceId, TargetKind,
};

use crate::configuration::MQTT_TARGET_KIND;

const MAX_TOPIC_BYTES: usize = 65_535;
const MAX_CLIENT_ID_BYTES: usize = 128;

/// Trusted, versioned MQTT namespace shared by every outbound mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopicNamespace {
    environment: Environment,
    bridge_id: BridgeId,
    base: String,
}

impl TopicNamespace {
    pub(crate) fn new(
        environment: Environment,
        bridge_id: &BridgeId,
    ) -> Result<Self, ConfigurationError> {
        let base = format!(
            "uob/v1/{}/{}",
            environment_name(environment),
            encode_segment(bridge_id.as_str())
        );
        validate_length(&base, "bridge_id")?;
        validate_length(&format!("{base}/availability"), "bridge_id")?;
        Ok(Self {
            environment,
            bridge_id: bridge_id.clone(),
            base,
        })
    }

    /// Returns the exact version/environment/bridge prefix.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Returns the retained bridge availability topic.
    #[must_use]
    pub fn availability(&self) -> String {
        format!("{}/availability", self.base)
    }

    /// Returns the only command namespace this adapter subscribes to.
    #[must_use]
    pub fn command_subscription(&self) -> String {
        format!("{}/commands/+/+", self.base)
    }

    pub(crate) fn command_topic_matches(
        &self,
        topic: &str,
        resource: &ResourceRef,
        request_id: &RequestId,
    ) -> bool {
        resource.bridge_id == self.bridge_id
            && topic
                == format!(
                    "{}/commands/{}/{}",
                    self.base,
                    encode_segment(resource.station_id.as_str()),
                    encode_segment(request_id.as_str())
                )
    }

    pub(crate) fn command_result(
        &self,
        target: &TargetInstanceId,
        result: &CommandResult,
        maximum_payload_bytes: usize,
    ) -> Result<WirePublication, MappingError> {
        if result.schema_version != ContractVersion::V1_INITIAL
            || result.resource.bridge_id != self.bridge_id
            || !matches!(
                &result.return_route.origin,
                uob_contracts::AuthenticatedCommandOrigin::Target {
                    target_instance_id,
                    ..
                } if target_instance_id == target
            )
        {
            return Err(MappingError::IdentityMismatch);
        }
        Ok(WirePublication {
            topic: self.message_topic(
                "results",
                result.resource.station_id.as_str(),
                Some(result.return_route.request_id.as_str()),
            )?,
            retain: false,
            payload: encode(result, maximum_payload_bytes)?,
        })
    }

    pub(crate) fn point_catalog(
        &self,
        snapshot: &StationSnapshot,
        maximum_payload_bytes: usize,
    ) -> Result<Vec<WirePublication>, MappingError> {
        crate::points::publications(&self.bridge_id, &self.base, snapshot, maximum_payload_bytes)
    }

    pub(crate) fn home_assistant_discovery(
        &self,
        snapshot: &StationSnapshot,
        maximum_payload_bytes: usize,
    ) -> Result<Vec<WirePublication>, MappingError> {
        crate::discovery::publications(
            self.environment,
            &self.bridge_id,
            &self.base,
            snapshot,
            maximum_payload_bytes,
        )
    }

    /// Derives a process-controlled client ID with no user-supplied override.
    pub(crate) fn client_id(
        &self,
        target: &TargetInstanceId,
    ) -> Result<String, ConfigurationError> {
        let value = format!(
            "uob-v1-{}-{}-{}",
            environment_name(self.environment),
            encode_segment(self.bridge_id.as_str()),
            encode_segment(target.as_str())
        );
        if value.len() > MAX_CLIENT_ID_BYTES {
            return Err(ConfigurationError::field(
                ConfigurationErrorCode::InvalidField,
                "target_instance_id",
            ));
        }
        Ok(value)
    }

    pub(crate) fn availability_publication(
        &self,
        target: &TargetInstanceId,
        online: bool,
    ) -> WirePublication {
        let payload = serde_json::to_vec(&AvailabilityPayload {
            schema_version: ContractVersion::V1_INITIAL,
            environment: self.environment,
            bridge_id: &self.bridge_id,
            target_instance_id: target,
            status: if online { "online" } else { "offline" },
        })
        .expect("availability payload contains only infallible canonical fields");
        WirePublication {
            topic: self.availability(),
            retain: true,
            payload,
        }
    }

    pub(crate) fn map<E: Serialize>(
        &self,
        target: &TargetInstanceId,
        revision: u64,
        delivery: &TargetDelivery<E>,
        maximum_payload_bytes: usize,
    ) -> Result<WirePublication, MappingError> {
        if &delivery.target_instance_id != target
            || delivery.target_configuration_revision != revision
            || delivery.station_ordering_key.bridge_id != self.bridge_id
        {
            return Err(MappingError::IdentityMismatch);
        }
        let (category, station_id, identity, retain, payload) = match delivery.message.as_ref() {
            TargetMessage::StationSnapshot(snapshot) => {
                if snapshot.schema_version != ContractVersion::V1_INITIAL
                    || snapshot.station != delivery.station_ordering_key
                {
                    return Err(MappingError::IdentityMismatch);
                }
                (
                    "state",
                    snapshot.station.station_id.as_str(),
                    None,
                    true,
                    encode(snapshot, maximum_payload_bytes)?,
                )
            }
            TargetMessage::DomainEvent(event) => {
                if event.schema_version != ContractVersion::V1_INITIAL
                    || event.runtime.environment != self.environment
                    || event.resource != delivery.station_ordering_key
                {
                    return Err(MappingError::IdentityMismatch);
                }
                (
                    "events",
                    event.resource.station_id.as_str(),
                    Some(event.event_id.as_str()),
                    false,
                    encode(event, maximum_payload_bytes)?,
                )
            }
            TargetMessage::CommandResult(result) => {
                if result.schema_version != ContractVersion::V1_INITIAL
                    || result.resource != delivery.station_ordering_key
                {
                    return Err(MappingError::IdentityMismatch);
                }
                (
                    "results",
                    result.resource.station_id.as_str(),
                    Some(result.return_route.request_id.as_str()),
                    false,
                    encode(result, maximum_payload_bytes)?,
                )
            }
            TargetMessage::Diagnostic(trace) => {
                if trace.schema_version != ContractVersion::V1_INITIAL
                    || trace.target.as_ref().is_some_and(|trace_target| {
                        trace_target.instance_id != *target
                            || trace_target.kind
                                != TargetKind::new(MQTT_TARGET_KIND).expect("static target kind")
                    })
                {
                    return Err(MappingError::IdentityMismatch);
                }
                (
                    "traces",
                    delivery.station_ordering_key.station_id.as_str(),
                    Some(trace.trace_id.as_str()),
                    false,
                    encode(trace, maximum_payload_bytes)?,
                )
            }
        };
        let topic = self.message_topic(category, station_id, identity)?;
        Ok(WirePublication {
            topic,
            retain,
            payload,
        })
    }

    fn message_topic(
        &self,
        category: &str,
        station_id: &str,
        identity: Option<&str>,
    ) -> Result<String, MappingError> {
        let mut topic = format!("{}/{}/{}", self.base, category, encode_segment(station_id));
        if let Some(identity) = identity {
            topic.push('/');
            topic.push_str(&encode_segment(identity));
        }
        if topic.len() > MAX_TOPIC_BYTES {
            return Err(MappingError::TopicTooLong);
        }
        Ok(topic)
    }
}

pub(crate) fn encode(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>, MappingError> {
    let mut output = BoundedBytes::new(maximum);
    match serde_json::to_writer(&mut output, value) {
        Ok(()) => Ok(output.into_inner()),
        Err(_) if output.exceeded => Err(MappingError::PayloadTooLarge),
        Err(_) => Err(MappingError::EncodingFailed),
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

impl BoundedBytes {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(maximum.min(8 * 1024)),
            maximum,
            exceeded: false,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl io::Write for BoundedBytes {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("MQTT payload size overflow"));
        };
        if next > self.maximum {
            self.exceeded = true;
            return Err(io::Error::other("MQTT payload exceeds bound"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WirePublication {
    pub(crate) topic: String,
    pub(crate) retain: bool,
    pub(crate) payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MappingError {
    IdentityMismatch,
    EncodingFailed,
    PayloadTooLarge,
    TopicTooLong,
}

impl MappingError {
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::IdentityMismatch => "mqtt.canonical_identity_mismatch",
            Self::EncodingFailed => "mqtt.payload_encoding_failed",
            Self::PayloadTooLarge => "mqtt.payload_too_large",
            Self::TopicTooLong => "mqtt.topic_too_long",
        }
    }
}

#[derive(Serialize)]
struct AvailabilityPayload<'a> {
    schema_version: ContractVersion,
    environment: Environment,
    bridge_id: &'a BridgeId,
    target_instance_id: &'a TargetInstanceId,
    status: &'static str,
}

fn environment_name(environment: Environment) -> &'static str {
    match environment {
        Environment::Production => "production",
        Environment::Staging => "staging",
        Environment::Demo => "demo",
    }
}

pub(crate) fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

fn validate_length(value: &str, field: &'static str) -> Result<(), ConfigurationError> {
    if value.len() > MAX_TOPIC_BYTES {
        Err(ConfigurationError::field(
            ConfigurationErrorCode::InvalidField,
            field,
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use uob_contracts::{BridgeId, Environment};

    use super::{MAX_TOPIC_BYTES, TopicNamespace, encode_segment};

    #[test]
    fn topic_segments_are_injective_and_cannot_create_wildcards_or_levels() {
        assert_eq!(encode_segment("safe-id"), "safe-id");
        assert_eq!(encode_segment("a/b+#%\n"), "a%2Fb%2B%23%25%0A");
        assert_ne!(encode_segment("a/b"), encode_segment("a%2Fb"));
    }

    #[test]
    fn namespace_rejects_a_bridge_that_only_overflows_the_availability_topic() {
        let prefix_bytes = "uob/v1/demo/".len();
        let bridge = BridgeId::new("a".repeat(MAX_TOPIC_BYTES - prefix_bytes))
            .expect("nonempty bridge identity");

        assert!(TopicNamespace::new(Environment::Demo, &bridge).is_err());
    }
}
