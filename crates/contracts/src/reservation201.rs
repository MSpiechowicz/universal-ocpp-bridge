//! Reference-only OCPP 2.0.1 reservation requests and value-free native/reconciliation evidence.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const RESERVE_NOW_REFERENCE_SCHEMA_201: &str = "urn:uob:ocpp201:ReserveNowReference:1";
pub const CANCEL_RESERVATION_SCHEMA_201: &str = "urn:OCPP:Cp:2:2020:3:CancelReservationRequest";
pub const RESERVATION_REFERENCE_PREFIX_201: &str = "reserve201:";

#[must_use]
pub fn valid_reservation_reference_201(value: &str) -> bool {
    value.len() == 75
        && value.starts_with(RESERVATION_REFERENCE_PREFIX_201)
        && value.as_bytes()[11..].iter().all(u8::is_ascii_hexdigit)
}

/// Native `ConnectorEnumType`; carried only as an exact optional reservation constraint.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum ReservationConnectorType201 {
    #[serde(rename = "cCCS1")]
    CCcs1,
    #[serde(rename = "cCCS2")]
    CCcs2,
    #[serde(rename = "cG105")]
    CG105,
    #[serde(rename = "cTesla")]
    CTesla,
    #[serde(rename = "cType1")]
    CType1,
    #[serde(rename = "cType2")]
    CType2,
    #[serde(rename = "s309-1P-16A")]
    S309OnePhase16A,
    #[serde(rename = "s309-1P-32A")]
    S309OnePhase32A,
    #[serde(rename = "s309-3P-16A")]
    S309ThreePhase16A,
    #[serde(rename = "s309-3P-32A")]
    S309ThreePhase32A,
    #[serde(rename = "sBS1361")]
    SBs1361,
    #[serde(rename = "sCEE-7-7")]
    SCee77,
    #[serde(rename = "sType2")]
    SType2,
    #[serde(rename = "sType3")]
    SType3,
    Other1PhMax16A,
    Other1PhOver16A,
    Other3Ph,
    Pan,
    #[serde(rename = "wInductive")]
    WInductive,
    #[serde(rename = "wResonant")]
    WResonant,
    Undetermined,
    Unknown,
}

/// Public request wrapper. Raw `idToken`/`groupIdToken` stay in the owner-only provider file.
#[derive(Clone, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveNowReference201 {
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub id: i32,
    pub expiry_date_time: UtcTimestamp,
    /// Absent means an unspecified EVSE, which requires explicit `NonEvseSpecific` support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = i32::MAX))]
    pub evse_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_type: Option<ReservationConnectorType201>,
    #[schemars(
        length(min = 75, max = 75),
        regex(pattern = "^reserve201:[0-9a-fA-F]{64}$")
    )]
    pub reservation_reference: String,
}

impl std::fmt::Debug for ReserveNowReference201 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReserveNowReference201")
            .field("id", &self.id)
            .field("expiry_date_time", &self.expiry_date_time)
            .field("evse_id", &self.evse_id)
            .field("connector_type", &self.connector_type)
            .field("reservation_reference", &"[redacted]")
            .finish()
    }
}

impl<'de> Deserialize<'de> for ReserveNowReference201 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Request {
            id: i32,
            expiry_date_time: UtcTimestamp,
            #[serde(default)]
            evse_id: Option<u32>,
            #[serde(default)]
            connector_type: Option<ReservationConnectorType201>,
            reservation_reference: String,
        }
        let value = Request::deserialize(deserializer)?;
        if !valid_reservation_reference_201(&value.reservation_reference) {
            return Err(serde::de::Error::custom(
                "invalid protected reservation reference",
            ));
        }
        if value
            .evse_id
            .is_some_and(|id| id == 0 || i32::try_from(id).is_err())
        {
            return Err(serde::de::Error::custom("invalid reservation EVSE"));
        }
        Ok(Self {
            id: value.id,
            expiry_date_time: value.expiry_date_time,
            evse_id: value.evse_id,
            connector_type: value.connector_type,
            reservation_reference: value.reservation_reference,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ReserveNowStatus201 {
    Accepted,
    Faulted,
    Occupied,
    Rejected,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum CancelReservationStatus201 {
    Accepted,
    Rejected,
}

/// `Removed` is the station's explicit `ReservationStatusUpdate`, never an inferred status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState201 {
    Pending,
    Active,
    Uncertain,
    Rejected,
    Consumed,
    Cancelled,
    Expired,
    Removed,
    Superseded,
    Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationReconciliation201 {
    pub revision: u64,
    pub state: ReservationState201,
    pub observed_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_time: Option<UtcTimestamp>,
}

/// Does not carry identities, groups, matching keys, protected references or native statusInfo.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum ReservationResult201 {
    ReserveNow {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        reservation_id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = 1, max = i32::MAX))]
        evse_id: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ReserveNowStatus201>,
        reconciliation: ReservationReconciliation201,
    },
    CancelReservation {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        reservation_id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<CancelReservationStatus201>,
        reconciliation: ReservationReconciliation201,
    },
}

impl ReservationResult201 {
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(
            self,
            Self::ReserveNow {
                status: Some(ReserveNowStatus201::Accepted),
                ..
            } | Self::CancelReservation {
                status: Some(CancelReservationStatus201::Accepted),
                ..
            }
        )
    }
    #[must_use]
    pub fn has_native_status(&self) -> bool {
        match self {
            Self::ReserveNow { status, .. } => status.is_some(),
            Self::CancelReservation { status, .. } => status.is_some(),
        }
    }
    pub fn reconciliation_mut(&mut self) -> &mut ReservationReconciliation201 {
        match self {
            Self::ReserveNow { reconciliation, .. }
            | Self::CancelReservation { reconciliation, .. } => reconciliation,
        }
    }
}
