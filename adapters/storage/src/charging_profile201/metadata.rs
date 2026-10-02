//! SQLite-only serde forms. Application metadata stays typed and serialization borrows it.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uob_application::{
    ProfileFootprint201, ProfileMutation201, ProfileReservation201, StorageError, StorageErrorCode,
};
use uob_contracts::{ChargingProfilePurpose201, ClearChargingProfileRequest201, UtcTimestamp};

#[derive(Deserialize, Serialize)]
#[serde(remote = "ProfileFootprint201")]
struct FootprintData201 {
    id: i32,
    evse_id: i32,
    purpose: ChargingProfilePurpose201,
    stack_level: i32,
    transaction_id: Option<String>,
    valid_from: Option<UtcTimestamp>,
    valid_to: Option<UtcTimestamp>,
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "ProfileMutation201")]
enum MutationData201 {
    Set {
        #[serde(with = "FootprintData201")]
        footprint: ProfileFootprint201,
        full_native: bool,
    },
    Clear(ClearChargingProfileRequest201),
}

#[derive(Deserialize)]
struct Footprint201(#[serde(with = "FootprintData201")] ProfileFootprint201);

#[derive(Serialize)]
struct FootprintRef201<'a>(#[serde(with = "FootprintData201")] &'a ProfileFootprint201);

/// Only bounded mutation metadata is duplicated in JSON; identity remains in SQL columns.
#[derive(Deserialize, Serialize)]
pub(super) struct Mutation201 {
    generation: u64,
    requires_baseline: bool,
    #[serde(with = "MutationData201")]
    pub(super) mutation: ProfileMutation201,
}

#[derive(Serialize)]
struct MutationRef201<'a> {
    generation: u64,
    requires_baseline: bool,
    #[serde(with = "MutationData201")]
    mutation: &'a ProfileMutation201,
}

pub(super) fn encode_footprint(value: &ProfileFootprint201) -> Result<String, StorageError> {
    encode(&FootprintRef201(value))
}

pub(super) fn decode_footprint(value: &str) -> Result<ProfileFootprint201, StorageError> {
    decode::<Footprint201>(value).map(|value| value.0)
}

pub(super) fn encode_mutation(value: &ProfileReservation201) -> Result<String, StorageError> {
    encode(&MutationRef201 {
        generation: value.generation,
        requires_baseline: value.requires_baseline,
        mutation: &value.mutation,
    })
}

pub(super) fn decode_mutation(value: &str) -> Result<Mutation201, StorageError> {
    decode(value)
}

fn encode<T: Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "profile metadata encoding failed",
        )
    })
}

fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "profile metadata invalid",
        )
    })
}
