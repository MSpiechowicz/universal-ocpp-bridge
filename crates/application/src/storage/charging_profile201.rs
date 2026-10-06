//! Durable ownership metadata, independent of prunable command history and full schedules.
use super::StorageFuture;
use uob_contracts::{
    ChargingProfilePurpose201, ClearChargingProfileRequest201, CorrelationId, RequestId,
    ResourceRef, SetChargingProfileRequest201, UtcTimestamp,
};

pub const MAX_PROFILE_FOOTPRINTS_201: usize = 128;

/// Conservative ownership footprint. Uncertain replacements retain both old and candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileFootprint201 {
    pub id: i32,
    pub evse_id: i32,
    pub purpose: ChargingProfilePurpose201,
    pub stack_level: i32,
    pub transaction_id: Option<String>,
    pub valid_from: Option<UtcTimestamp>,
    pub valid_to: Option<UtcTimestamp>,
}
impl From<&SetChargingProfileRequest201> for ProfileFootprint201 {
    fn from(request: &SetChargingProfileRequest201) -> Self {
        let profile = &request.charging_profile;
        Self {
            id: profile.id,
            evse_id: request.evse_id,
            purpose: profile.charging_profile_purpose,
            stack_level: profile.stack_level,
            transaction_id: profile.transaction_id.clone(),
            valid_from: profile.valid_from,
            valid_to: profile.valid_to,
        }
    }
}
impl ProfileFootprint201 {
    /// FR39 has no validity qualifier. Cross-scope `TxDefault` ownership is also prohibited.
    #[must_use]
    pub fn conflicts(&self, other: &Self) -> bool {
        if self.id == other.id
            || self.purpose != other.purpose
            || self.stack_level != other.stack_level
        {
            return false;
        }
        if self.purpose == ChargingProfilePurpose201::TxProfile
            && self.transaction_id == other.transaction_id
        {
            return true;
        }
        let same_scope = self.evse_id == other.evse_id;
        let station_default = self.purpose == ChargingProfilePurpose201::TxDefaultProfile
            && self.evse_id != other.evse_id
            && (self.evse_id == 0 || other.evse_id == 0);
        station_default || (same_scope && self.overlaps(other))
    }
    fn overlaps(&self, other: &Self) -> bool {
        !matches!((self.valid_to, other.valid_from), (Some(to), Some(from)) if to <= from)
            && !matches!((other.valid_to, self.valid_from), (Some(to), Some(from)) if to <= from)
    }
    #[must_use]
    pub fn matches_clear(&self, request: &ClearChargingProfileRequest201) -> bool {
        if let Some(id) = request.charging_profile_id {
            return id == self.id;
        }
        request
            .charging_profile_criteria
            .as_ref()
            .is_some_and(|criteria| {
                criteria.evse_id.is_none_or(|evse| evse == self.evse_id)
                    && criteria
                        .stack_level
                        .is_none_or(|level| level == self.stack_level)
                    && criteria
                        .charging_profile_purpose
                        .is_none_or(|purpose| purpose == self.purpose)
            })
    }
}

/// Mutation captured before admission; canonical producers do not gain full native evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileMutation201 {
    Set {
        footprint: ProfileFootprint201,
        full_native: bool,
    },
    Clear(ClearChargingProfileRequest201),
}

/// Admission and reservation commit in the same atomic transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileReservation201 {
    pub request_id: RequestId,
    pub station: ResourceRef,
    pub connection: CorrelationId,
    pub generation: u64,
    pub requires_baseline: bool,
    pub mutation: ProfileMutation201,
}

/// Bounded, typed local metadata view; it is not a discovered station inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileOwnership201 {
    pub baseline: [bool; 3],
    pub footprints: Vec<ProfileFootprint201>,
    pub busy: bool,
}

/// Durable ledger state of one footprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileOwnershipState201 {
    /// Admitted and possibly in flight; no native answer is recorded yet.
    Reserved,
    /// The station accepted the owner's request.
    Owned,
    /// The outcome is unknown and awaits explicit reconciliation.
    Uncertain,
}

/// One footprint with the request that owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileOwner201 {
    pub request_id: RequestId,
    pub state: ProfileOwnershipState201,
    pub footprint: ProfileFootprint201,
}

/// Owner-attributed ledger view for exact checks; it is not a discovered station inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileOwners201 {
    pub owners: Vec<ProfileOwner201>,
    /// A station mutation is admitted and not yet finished.
    pub busy: bool,
}

/// Implemented on the same bounded storage worker as atomic writes. No default/no-op adapter.
pub trait ChargingProfileStore201: Send + Sync {
    fn charging_profile_ownership(
        &self,
        station: ResourceRef,
    ) -> StorageFuture<'_, ProfileOwnership201>;
    /// The same bounded ledger with each footprint's owner request and state.
    fn charging_profile_owners(&self, station: ResourceRef) -> StorageFuture<'_, ProfileOwners201>;
    /// Composition startup only, while holding exclusive ownership of the operational state.
    fn interrupt_charging_profile_mutations(&self) -> StorageFuture<'_, ()>;
}

/// A purpose-only all-EVSE Clear is the sole explicit baseline proof, including Unknown.
#[must_use]
pub fn baseline_purpose(
    request: &ClearChargingProfileRequest201,
) -> Option<ChargingProfilePurpose201> {
    if request.charging_profile_id.is_some() {
        return None;
    }
    let criteria = request.charging_profile_criteria.as_ref()?;
    if criteria.evse_id.is_some() || criteria.stack_level.is_some() {
        return None;
    }
    criteria.charging_profile_purpose
}

#[must_use]
pub const fn purpose_index(purpose: ChargingProfilePurpose201) -> usize {
    match purpose {
        ChargingProfilePurpose201::ChargingStationMaxProfile => 0,
        ChargingProfilePurpose201::TxDefaultProfile => 1,
        ChargingProfilePurpose201::TxProfile => 2,
    }
}
