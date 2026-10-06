//! Reference-only reservation requests and value-free native/reconciliation evidence.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const RESERVE_NOW_REFERENCE_SCHEMA_16: &str = "urn:uob:ocpp16:ReserveNowReference:1";
pub const RESERVATION_REFERENCE_PREFIX_16: &str = "reserve16:";

#[must_use]
pub fn valid_reservation_reference_16(value: &str) -> bool {
    value.len() == 74
        && value.starts_with(RESERVATION_REFERENCE_PREFIX_16)
        && value.as_bytes()[10..].iter().all(u8::is_ascii_hexdigit)
}

#[derive(Clone, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveNowReference16 {
    #[schemars(range(min = 0, max = u32::MAX))]
    pub connector_id: u32,
    pub expiry_date: UtcTimestamp,
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub reservation_id: i32,
    #[schemars(
        length(min = 74, max = 74),
        regex(pattern = "^reserve16:[0-9a-fA-F]{64}$")
    )]
    pub reservation_reference: String,
}

impl std::fmt::Debug for ReserveNowReference16 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReserveNowReference16")
            .field("connector_id", &self.connector_id)
            .field("expiry_date", &self.expiry_date)
            .field("reservation_id", &self.reservation_id)
            .field("reservation_reference", &"[redacted]")
            .finish()
    }
}

impl<'de> Deserialize<'de> for ReserveNowReference16 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Request {
            connector_id: u32,
            expiry_date: UtcTimestamp,
            reservation_id: i32,
            reservation_reference: String,
        }
        let value = Request::deserialize(deserializer)?;
        if !valid_reservation_reference_16(&value.reservation_reference) {
            return Err(serde::de::Error::custom(
                "invalid protected reservation reference",
            ));
        }
        Ok(Self {
            connector_id: value.connector_id,
            expiry_date: value.expiry_date,
            reservation_id: value.reservation_id,
            reservation_reference: value.reservation_reference,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ReserveNowStatus16 {
    Accepted,
    Faulted,
    Occupied,
    Rejected,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum CancelReservationStatus16 {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState16 {
    Pending,
    Active,
    Uncertain,
    Rejected,
    Consumed,
    Cancelled,
    Expired,
    Faulted,
    Unavailable,
    Superseded,
    Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationReconciliation16 {
    pub revision: u64,
    pub state: ReservationState16,
    pub observed_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_time: Option<UtcTimestamp>,
}

/// Does not carry identities, groups, matching keys, or protected references.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum ReservationResult16 {
    ReserveNow {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        reservation_id: i32,
        #[schemars(range(min = 0, max = u32::MAX))]
        connector_id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ReserveNowStatus16>,
        reconciliation: ReservationReconciliation16,
    },
    CancelReservation {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        reservation_id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<CancelReservationStatus16>,
        reconciliation: ReservationReconciliation16,
    },
}

impl ReservationResult16 {
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(
            self,
            Self::ReserveNow {
                status: Some(ReserveNowStatus16::Accepted),
                ..
            } | Self::CancelReservation {
                status: Some(CancelReservationStatus16::Accepted),
                ..
            }
        )
    }
    pub fn reconciliation_mut(&mut self) -> &mut ReservationReconciliation16 {
        match self {
            Self::ReserveNow { reconciliation, .. }
            | Self::CancelReservation { reconciliation, .. } => reconciliation,
        }
    }
}
