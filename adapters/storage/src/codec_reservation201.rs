use serde::{Deserialize, Serialize};
use uob_application::{
    ReservationCandidate201, ReservationKey201, ReservationRecord201, StorageError,
    StorageErrorCode,
};
use uob_contracts::{
    RequestId, ReservationConnectorType201, ReservationState201, ResourceRef, UtcTimestamp,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    evse_id: Option<u32>,
    connector_type: Option<ReservationConnectorType201>,
    expiry_date_time: UtcTimestamp,
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
    state: ReservationState201,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    source_time: Option<UtcTimestamp>,
    source_observed_at: Option<UtcTimestamp>,
    started: bool,
    unresolved: bool,
    ambiguous: bool,
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
    evse_id: Option<u32>,
    connector_type: Option<ReservationConnectorType201>,
    expiry_date_time: UtcTimestamp,
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
    state: ReservationState201,
    admitted_at: UtcTimestamp,
    changed_at: UtcTimestamp,
    source_time: Option<UtcTimestamp>,
    source_observed_at: Option<UtcTimestamp>,
    started: bool,
    unresolved: bool,
    ambiguous: bool,
    history_floor: Option<UtcTimestamp>,
}
pub(crate) fn encode(value: &ReservationRecord201) -> Result<String, StorageError> {
    serde_json::to_string(&EncodedRecord {
        station: &value.station,
        request_id: &value.request_id,
        reservation_id: value.reservation_id,
        revision: value.revision,
        candidate: value.candidate.as_ref().map(|c| EncodedCandidate {
            evse_id: c.evse_id,
            connector_type: c.connector_type,
            expiry_date_time: c.expiry_date_time,
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
pub(crate) fn decode(payload: &str) -> Result<ReservationRecord201, StorageError> {
    let value: Record = serde_json::from_str(payload).map_err(|_| error())?;
    if value.revision == 0
        || value.revision > i64::MAX as u64
        || !crate::reservation201::validation::station_scope(&value.station)
        || value.changed_at < value.admitted_at
        || value.state == ReservationState201::Ambiguous
        || value.candidate.as_ref().is_some_and(|c| {
            c.expiry_date_time <= value.admitted_at
                || c.evse_id
                    .is_some_and(|id| id == 0 || i32::try_from(id).is_err())
        })
    {
        return Err(error());
    }
    Ok(ReservationRecord201 {
        station: value.station,
        request_id: value.request_id,
        reservation_id: value.reservation_id,
        revision: value.revision,
        candidate: value.candidate.map(|c| ReservationCandidate201 {
            evse_id: c.evse_id,
            connector_type: c.connector_type,
            expiry_date_time: c.expiry_date_time,
            token_key: ReservationKey201(c.token_key),
            group_key: c.group_key.map(ReservationKey201),
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
