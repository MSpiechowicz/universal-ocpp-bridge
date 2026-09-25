use rusqlite::{Connection, TransactionBehavior, params};
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{CommandResult, ConfigurationObservation, ConfigurationResult};

use crate::{configuration::unavailable, recovery};

pub(super) fn append_configuration_observation(
    connection: &mut Connection,
    write_id: &str,
    observation: ConfigurationObservation,
) -> Result<Option<CommandResult>, StorageError> {
    // The immediate transaction serializes this read/merge/write with other SQLite handles
    // (including handles in other processes), not only requests on this worker's queue.
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let Some(mut result) = recovery::command_result(&transaction, write_id)? else {
        return Ok(None);
    };
    let Some(ConfigurationResult::Write { key, .. }) = result.configuration.as_ref() else {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "configuration observation requires a completed write",
        ));
    };
    if key != &observation.key.key || result.return_route.request_id.as_str() != write_id {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "configuration observation write identity mismatch",
        ));
    }
    if result
        .configuration_observations
        .iter()
        .any(|entry| entry.read_request_id == observation.read_request_id)
    {
        return Ok(Some(result));
    }
    if result.configuration_observations.len() >= 128 {
        return Err(StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "configuration observations exceed retained bound",
        ));
    }
    result.configuration_observations.push(observation);
    let payload = serde_json::to_string(&result).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "configuration observation serialization failed",
        )
    })?;
    transaction
        .execute(
            "UPDATE command_results SET payload = ?2 WHERE request_id = ?1",
            params![write_id, payload],
        )
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)?;
    Ok(Some(result))
}
