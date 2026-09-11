use crate::{DataPointValue, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version-specific durable facts, separate from observed charging and command acceptance.
/// Raw idTags are never stored here. Fingerprints refer to canonical validated wire payloads.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct Ocpp16TransactionEvidence {
    pub transaction_id: i32,
    pub start_message_id: String,
    pub start_fingerprint: String,
    pub authorization_status: String,
    pub authorization_expiry: Option<UtcTimestamp>,
    pub identity_reference: Option<String>,
    /// Native meter register in Wh; signed integer semantics are preserved.
    pub meter_start: i32,
    pub reservation_id: Option<i32>,
    pub stop_message_id: Option<String>,
    pub stop_fingerprint: Option<String>,
    pub stop_identity_fingerprint: Option<String>,
    pub meter_stop: Option<i32>,
    /// Absent means the OCPP default Local; explicit values retain their native spelling.
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transaction_data: Vec<DataPointValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signed_values: Vec<String>,
}
