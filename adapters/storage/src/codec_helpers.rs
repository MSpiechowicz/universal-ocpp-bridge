use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    AuthorizationState, Durability, OPERATIONAL_HISTORY_RETENTION_SECONDS, StorageError,
    StorageErrorCode,
};
use uob_contracts::StationSnapshot;

pub(super) fn encode_snapshot(value: &StationSnapshot) -> Result<(String, String), StorageError> {
    Ok((
        crate::snapshots::station_key(&value.station)?,
        json(&value)?,
    ))
}

pub(super) fn json<T: Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "record serialization failed",
        )
    })
}

pub(super) fn from_json<T: DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|_| corrupt("committed record failed typed decoding"))
}

pub(super) fn unsigned(value: u64, label: &str) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            format!("{label} exceeds SQLite integer range"),
        )
    })
}

pub(super) fn signed(value: i64, label: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| corrupt(&format!("negative {label}")))
}

pub(super) fn retention_boundary(value: uob_contracts::UtcTimestamp) -> Result<i64, StorageError> {
    value
        .into_inner()
        .unix_timestamp()
        .checked_add(OPERATIONAL_HISTORY_RETENTION_SECONDS)
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorCode::InvalidRequest,
                "operational retention timestamp exceeds supported range",
            )
        })
}

pub(super) const fn durability(value: Durability) -> i64 {
    match value {
        Durability::Critical => 0,
        Durability::BestEffortTelemetry => 1,
    }
}

pub(super) const fn durability_or_state(value: AuthorizationState) -> i64 {
    match value {
        AuthorizationState::Active => 0,
        AuthorizationState::Revoked => 1,
    }
}

pub(super) fn decode_durability(value: i64) -> Result<Durability, StorageError> {
    match value {
        0 => Ok(Durability::Critical),
        1 => Ok(Durability::BestEffortTelemetry),
        _ => Err(corrupt("unknown durability value")),
    }
}

pub(super) fn corrupt(detail: &str) -> StorageError {
    StorageError::new(StorageErrorCode::IntegrityFailure, detail)
}

pub(super) fn integrity(error: impl std::fmt::Display) -> StorageError {
    corrupt(&error.to_string())
}
