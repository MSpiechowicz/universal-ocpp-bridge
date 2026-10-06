//! Application-owned reservation lifecycle and atomic mutation/observation boundaries.
use crate::StorageFuture;
use uob_contracts::{RequestId, ReservationState16, ResourceRef, UtcTimestamp};
pub(crate) mod status;

pub const MAX_RESERVATION_REVISIONS_16: usize = 256;

/// One-way reservation-specific `CiString` digest, never an authorization identity.
#[derive(Clone, Eq, PartialEq)]
pub struct ReservationKey16(pub [u8; 32]);
impl std::fmt::Debug for ReservationKey16 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReservationKey16([protected])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationCandidate16 {
    pub connector_id: u32,
    pub expiry_date: UtcTimestamp,
    pub token_key: ReservationKey16,
    pub group_key: Option<ReservationKey16>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReservationMutationKind16 {
    Reserve(ReservationCandidate16),
    Cancel,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationMutation16 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub reservation_id: i32,
    pub admitted_at: UtcTimestamp,
    pub generation: u64,
    pub mutation: ReservationMutationKind16,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationRecord16 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub reservation_id: i32,
    pub revision: u64,
    pub candidate: Option<ReservationCandidate16>,
    pub state: ReservationState16,
    pub admitted_at: UtcTimestamp,
    pub changed_at: UtcTimestamp,
    pub source_time: Option<UtcTimestamp>,
    /// Trusted receipt of the last source fact, separate from the ownership interval.
    pub source_observed_at: Option<UtcTimestamp>,
    pub started: bool,
    pub unresolved: bool,
    /// Retained attribution ambiguity; native ownership state remains independently known.
    pub ambiguous: bool,
    /// Latest discarded terminal interval, carried by the monotonic station head.
    pub history_floor: Option<UtcTimestamp>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReservationObservationKind16 {
    Start {
        reservation_id: i32,
        connector_id: u32,
        token_key: ReservationKey16,
        group_key: Option<ReservationKey16>,
        source_time: UtcTimestamp,
    },
    Status {
        connector_id: u32,
        state: ReservationState16,
        source_time: Option<UtcTimestamp>,
        any_eligible: bool,
    },
    Expiry,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationObservation16 {
    pub station: ResourceRef,
    pub observed_at: UtcTimestamp,
    pub kind: ReservationObservationKind16,
}

/// Same worker and transaction boundary as commands and transaction observations.
pub trait ReservationStore16: Send + Sync {
    fn reservations_16(&self, station: ResourceRef) -> StorageFuture<'_, Vec<ReservationRecord16>>;
    /// Startup maintenance only. Does not return or queue native mutators.
    fn recover_reservations_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
    fn expire_reservations_16(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
}

#[must_use]
pub fn reservation_matches_16(
    candidate: &ReservationCandidate16,
    connector: u32,
    token: &ReservationKey16,
    group: Option<&ReservationKey16>,
) -> bool {
    (candidate.connector_id == 0 || candidate.connector_id == connector)
        && (&candidate.token_key == token
            || candidate
                .group_key
                .as_ref()
                .zip(group)
                .is_some_and(|(a, b)| a == b))
}
#[must_use]
pub fn reservation_live_16(state: ReservationState16) -> bool {
    matches!(
        state,
        ReservationState16::Pending | ReservationState16::Active | ReservationState16::Uncertain
    )
}
