//! Application-owned OCPP 2.0.1 reservation lifecycle and atomic mutation/observation boundaries.
use crate::{
    AtomicStoreWrite, ObservationCommitError, OperationalStore, StorageFuture, registration,
};
use uob_contracts::{
    RequestId, ReservationConnectorType201, ReservationState201, ResourceRef, StationSnapshot,
    UtcTimestamp,
};

pub const MAX_RESERVATION_REVISIONS_201: usize = 256;

/// One-way digest of a native `IdTokenType` (type plus case-folded idToken), never an
/// authorization identity and never reversible to the presented token.
#[derive(Clone, Eq, PartialEq)]
pub struct ReservationKey201(pub [u8; 32]);
impl std::fmt::Debug for ReservationKey201 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReservationKey201([protected])")
    }
}

/// Absent `evse_id` is an unspecified-EVSE reservation; capacity is never bound to one EVSE.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationCandidate201 {
    pub evse_id: Option<u32>,
    pub connector_type: Option<ReservationConnectorType201>,
    pub expiry_date_time: UtcTimestamp,
    pub token_key: ReservationKey201,
    pub group_key: Option<ReservationKey201>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReservationMutationKind201 {
    Reserve(ReservationCandidate201),
    Cancel,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationMutation201 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub reservation_id: i32,
    pub admitted_at: UtcTimestamp,
    pub generation: u64,
    pub mutation: ReservationMutationKind201,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationRecord201 {
    pub station: ResourceRef,
    pub request_id: RequestId,
    pub reservation_id: i32,
    pub revision: u64,
    pub candidate: Option<ReservationCandidate201>,
    pub state: ReservationState201,
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

/// Native `ReservationUpdateStatusEnumType`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationUpdateStatus201 {
    Expired,
    Removed,
}
impl ReservationUpdateStatus201 {
    #[must_use]
    pub const fn state(self) -> ReservationState201 {
        match self {
            Self::Expired => ReservationState201::Expired,
            Self::Removed => ReservationState201::Removed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReservationObservationKind201 {
    /// Native `TransactionEvent.reservationId`: the station reports that this transaction
    /// terminates the reservation (H01.FR.15). Token evidence is optional in that event.
    Transaction {
        reservation_id: i32,
        evse_id: u32,
        token_key: Option<ReservationKey201>,
        group_key: Option<ReservationKey201>,
        source_time: UtcTimestamp,
    },
    /// Native `ReservationStatusUpdate` (H04.FR.01, H01.FR.16/17); it carries no timestamp.
    StatusUpdate {
        reservation_id: i32,
        status: ReservationUpdateStatus201,
    },
    /// Trusted bridge-clock expiry, applied even while the station is offline.
    Expiry,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationObservation201 {
    pub station: ResourceRef,
    pub observed_at: UtcTimestamp,
    pub kind: ReservationObservationKind201,
}

/// Same worker and transaction boundary as commands and transaction observations.
pub trait ReservationStore201: Send + Sync {
    fn reservations_201(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, Vec<ReservationRecord201>>;
    /// Startup maintenance only. Does not return or queue native mutators.
    fn recover_reservations_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
    fn expire_reservations_201(&self, now: UtcTimestamp) -> StorageFuture<'_, ()>;
}

/// An absent presented token leaves attribution to the station's explicit reservationId.
#[must_use]
pub fn reservation_matches_201(
    candidate: &ReservationCandidate201,
    evse_id: u32,
    token: Option<&ReservationKey201>,
    group: Option<&ReservationKey201>,
) -> bool {
    candidate.evse_id.is_none_or(|id| id == evse_id)
        && token.is_none_or(|token| {
            &candidate.token_key == token
                || candidate
                    .group_key
                    .as_ref()
                    .zip(group)
                    .is_some_and(|(a, b)| a == b)
        })
}
#[must_use]
pub fn reservation_live_201(state: ReservationState201) -> bool {
    matches!(
        state,
        ReservationState201::Pending | ReservationState201::Active | ReservationState201::Uncertain
    )
}

/// Commits one native `ReservationStatusUpdate` as an authoritative reservation fact.
/// The caller acknowledges the CALL only after this returns.
/// # Errors
/// Rejects unregistered stations and failed persistence without changing the snapshot.
pub async fn record_reservation_status_201<C, E, D, R>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &StationSnapshot,
    reservation_id: i32,
    status: ReservationUpdateStatus201,
    now: UtcTimestamp,
) -> Result<(), ObservationCommitError>
where
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    registration::v201::accepted(snapshot).map_err(|_| ObservationCommitError::InvalidState)?;
    let mut write = AtomicStoreWrite::empty();
    write.reservation_observations_201 = vec![ReservationObservation201 {
        station: snapshot.station.clone(),
        observed_at: now,
        kind: ReservationObservationKind201::StatusUpdate {
            reservation_id,
            status,
        },
    }];
    store
        .write_atomic(write)
        .await
        .map(|_| ())
        .map_err(ObservationCommitError::Storage)
}
