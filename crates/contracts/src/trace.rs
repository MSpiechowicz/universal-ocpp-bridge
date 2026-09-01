use std::{collections::BTreeMap, error::Error, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{ContractVersion, CorrelationId, ProcessInstanceId, TargetInstanceId, UtcTimestamp};

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

macro_rules! trace_text {
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
            /// Returns [`TraceIdentityError`] when `value` has no visible text.
            pub fn new(value: impl Into<String>) -> Result<Self, TraceIdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(TraceIdentityError(stringify!($name)));
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

trace_text!(
    TraceId,
    "Unique identity of one best-effort diagnostic trace record."
);
trace_text!(TraceStage, "Stable name of the diagnostic pipeline stage.");
trace_text!(TargetKind, "Stable registered kind of a target adapter.");

/// Failure to construct a trace identity or stable label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceIdentityError(&'static str);

impl fmt::Display for TraceIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cannot be empty", self.0)
    }
}

impl Error for TraceIdentityError {}

/// Monotonic best-effort trace sequence within one process instance.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct TraceSequence(pub u64);

/// Selected target identity attached only when a trace crosses a target boundary.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TraceTarget {
    /// Stable configured target instance.
    pub instance_id: TargetInstanceId,
    /// Stable registered target adapter kind.
    pub kind: TargetKind,
}

/// Direction of data or control at the named diagnostic stage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceDirection {
    /// Entering the bridge or current stage.
    Inbound,
    /// Leaving the bridge or current stage.
    Outbound,
    /// Internal decision or state transition without an external direction.
    Internal,
}

/// Sanitized diagnostic outcome; it is not a durable business result.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TraceOutcome {
    /// The traced stage completed as expected.
    Succeeded,
    /// The stage failed with an optional stable, sanitized reason code.
    Failed {
        /// Stable reason code, never a raw external error or credential.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Completion or delivery could not be established conclusively.
    Uncertain {
        /// Stable reason code, never a raw external error or credential.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Best-effort diagnostic data was intentionally shed or expired.
    Dropped {
        /// Stable reason code, never a raw external error or credential.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Optional payload details after mandatory redaction and bounding.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RedactedTraceDetails {
    /// Short sanitized description of the payload or state change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Bounded sanitized scalar fields; arbitrary nested payloads are not accepted.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    /// Whether source details were omitted to enforce capture limits.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// Versioned best-effort diagnostic metadata, separate from durable events and audit records.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TraceRecord {
    /// Version of this diagnostic contract.
    pub schema_version: ContractVersion,
    /// Unique identity of this trace record.
    pub trace_id: TraceId,
    /// Process that emitted this record.
    pub process_instance_id: ProcessInstanceId,
    /// Monotonic sequence within the process instance.
    pub trace_sequence: TraceSequence,
    /// Selected target identity, only for target-related stages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TraceTarget>,
    /// Correlation shared with related commands, events, and deliveries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    /// Directly causal trace record, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_trace_id: Option<TraceId>,
    /// Stable pipeline stage name.
    pub stage: TraceStage,
    /// Direction at the named stage.
    pub direction: TraceDirection,
    /// UTC instant at which the bridge observed this stage.
    pub observed_at: UtcTimestamp,
    /// Elapsed duration in microseconds, when a bounded operation completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_micros: Option<u64>,
    /// Sanitized diagnostic outcome.
    pub outcome: TraceOutcome,
    /// Optional details that were redacted before entering the diagnostic buffer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_details: Option<RedactedTraceDetails>,
}
