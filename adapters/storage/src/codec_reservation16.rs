use serde::{Deserialize, Serialize};
use uob_application::{
    ReservationCandidate16, ReservationKey16, ReservationRecord16, StorageError, StorageErrorCode,
};
use uob_contracts::{RequestId, ReservationState16, ResourceRef, UtcTimestamp};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    connector_id: u32,
    expiry_date: UtcTimestamp,
    token_key: [u8; 32],
    group_key: Option<[u8; 32]>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    station: ResourceRef,
    request_id: RequestId,
    reservation_id: i32,
    revision: u64,
    candidate: Option<Candidate>,
    state: ReservationState16,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    source_time: Option<UtcTimestamp>,
    #[serde(default)]
    source_observed_at: Option<UtcTimestamp>,
    started: bool,
    unresolved: bool,
    #[serde(default)]
    ambiguous: bool,
    #[serde(default)]
    history_floor: Option<UtcTimestamp>,
}
fn error() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "invalid reservation workflow record",
    )
}
#[derive(Serialize)]
struct EncodedCandidate<'a> {
    connector_id: u32,
    expiry_date: UtcTimestamp,
    token_key: &'a [u8; 32],
    group_key: Option<&'a [u8; 32]>,
}
#[derive(Serialize)]
struct EncodedRecord<'a> {
    station: &'a ResourceRef,
    request_id: &'a RequestId,
    reservation_id: i32,
    revision: u64,
    candidate: Option<EncodedCandidate<'a>>,
    state: ReservationState16,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    source_time: Option<UtcTimestamp>,
    source_observed_at: Option<UtcTimestamp>,
    started: bool,
    unresolved: bool,
    ambiguous: bool,
    history_floor: Option<UtcTimestamp>,
}
pub(crate) fn encode(value: &ReservationRecord16) -> Result<String, StorageError> {
    serde_json::to_string(&EncodedRecord {
        station: &value.station,
        request_id: &value.request_id,
        reservation_id: value.reservation_id,
        revision: value.revision,
        candidate: value.candidate.as_ref().map(|c| EncodedCandidate {
            connector_id: c.connector_id,
            expiry_date: c.expiry_date,
            token_key: &c.token_key.0,
            group_key: c.group_key.as_ref().map(|k| &k.0),
        }),
        state: value.state,
        admitted_at: value.admitted_at,
        changed_at: value.changed_at,
        source_time: value.source_time,
        source_observed_at: value.source_observed_at,
        started: value.started,
        unresolved: value.unresolved,
        ambiguous: value.ambiguous,
        history_floor: value.history_floor,
    })
    .map_err(|_| error())
}
pub(crate) fn decode(payload: &str) -> Result<ReservationRecord16, StorageError> {
    let value: Record = serde_json::from_str(payload).map_err(|_| error())?;
    if value.revision == 0
        || value.revision > i64::MAX as u64
        || value.station.resource.is_some()
        || !matches!(
            value.station.native_protocol_reference,
            None | Some(uob_contracts::NativeProtocolReference::Ocpp16 { connector_id: 0 })
        )
        || value.changed_at < value.admitted_at
        || value.state == ReservationState16::Ambiguous
        || value
            .candidate
            .as_ref()
            .is_some_and(|c| c.expiry_date <= value.admitted_at)
    {
        return Err(error());
    }
    Ok(ReservationRecord16 {
        station: value.station,
        request_id: value.request_id,
        reservation_id: value.reservation_id,
        revision: value.revision,
        candidate: value.candidate.map(|c| ReservationCandidate16 {
            connector_id: c.connector_id,
            expiry_date: c.expiry_date,
            token_key: ReservationKey16(c.token_key),
            group_key: c.group_key.map(ReservationKey16),
        }),
        state: value.state,
        admitted_at: value.admitted_at,
        changed_at: value.changed_at,
        source_time: value.source_time,
        source_observed_at: value.source_observed_at,
        started: value.started,
        unresolved: value.unresolved,
        ambiguous: value.ambiguous,
        history_floor: value.history_floor,
    })
}
