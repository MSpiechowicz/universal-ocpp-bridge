use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{ContractVersion, Environment, ResourceRef, RuntimeIdentity, UtcTimestamp};

macro_rules! event_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a value after rejecting empty or whitespace-only text.
            ///
            /// # Errors
            ///
            /// Returns [`EventIdentityError`] when `value` is empty.
            pub fn new(value: impl Into<String>) -> Result<Self, EventIdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(EventIdentityError(stringify!($name)));
                }
                Ok(Self(value))
            }

            /// Returns the stable wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

event_id!(EventId, "Globally unique identity of one durable event.");
event_id!(
    CorrelationId,
    "Stable identifier correlating related operations and events."
);
event_id!(EventType, "Versioned semantic name of an event payload.");

/// Error returned when constructing an event identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventIdentityError(&'static str);

impl fmt::Display for EventIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cannot be empty", self.0)
    }
}

impl std::error::Error for EventIdentityError {}

/// Trusted source class that originated an event.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventOrigin {
    /// Event derived from a charging station protocol exchange.
    Station,
    /// Event produced by an authorized management command.
    Management,
    /// Event produced by the bridge's internal recovery or policy logic.
    Bridge,
    /// Event replayed into an isolated staging environment.
    Replay,
}

/// Provenance retained when an event is replayed into another environment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct EventProvenance {
    /// Identity of the original event; never reused as the replay event ID.
    pub original_event_id: EventId,
    /// Trusted environment in which the original event was observed.
    pub original_environment: Environment,
}

/// Durable, versioned event with a statically typed payload.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct EventEnvelope<T> {
    /// Unique identity of this event occurrence.
    pub event_id: EventId,
    /// Version of this envelope and payload contract.
    pub schema_version: ContractVersion,
    /// Trusted process and artifact context at observation time.
    pub runtime: RuntimeIdentity,
    /// Stable logical resource, independent of runtime identity.
    pub resource: ResourceRef,
    /// Device/source timestamp, absent when the source supplied none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time: Option<UtcTimestamp>,
    /// UTC timestamp at which the bridge observed the event.
    pub observed_at: UtcTimestamp,
    /// Versioned semantic payload type.
    pub event_type: EventType,
    /// Trusted class of the event producer.
    pub origin: EventOrigin,
    /// Monotonic sequence within the resource's durable event stream.
    pub sequence: u64,
    /// Correlation identity shared with related operations, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    /// Identity of a directly causal event, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<EventId>,
    /// Original identity retained only when this event is a replay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EventProvenance>,
    /// Statically typed event data; raw target payloads are not part of the envelope.
    pub payload: T,
}

impl<T> EventEnvelope<T> {
    /// Converts an event into a staging replay with a new identity and provenance.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError::IdentityCollision`] if the caller attempts to reuse
    /// the original event identity, or [`ReplayError::NotStaging`] if the supplied
    /// runtime is not trusted staging context.
    pub fn into_staging_replay(
        self,
        replay_event_id: EventId,
        staging_runtime: RuntimeIdentity,
        observed_at: UtcTimestamp,
    ) -> Result<Self, ReplayError> {
        if replay_event_id == self.event_id {
            return Err(ReplayError::IdentityCollision);
        }
        if staging_runtime.environment != Environment::Staging {
            return Err(ReplayError::NotStaging);
        }

        let original_event_id = self.provenance.as_ref().map_or_else(
            || self.event_id.clone(),
            |value| value.original_event_id.clone(),
        );
        let original_environment = self
            .provenance
            .as_ref()
            .map_or(self.runtime.environment, |value| value.original_environment);

        Ok(Self {
            event_id: replay_event_id,
            runtime: staging_runtime,
            observed_at,
            origin: EventOrigin::Replay,
            causation_id: Some(self.event_id),
            provenance: Some(EventProvenance {
                original_event_id,
                original_environment,
            }),
            ..self
        })
    }
}

/// Failure to create an isolated staging replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayError {
    /// Replay identity would collide with the source event.
    IdentityCollision,
    /// Replay runtime was not a staging runtime.
    NotStaging,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityCollision => formatter.write_str("replay event ID must be new"),
            Self::NotStaging => formatter.write_str("replay runtime must be staging"),
        }
    }
}

impl std::error::Error for ReplayError {}
