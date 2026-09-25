use super::{CommandAdmissionError, CommandAdmissionErrorCode, StationCommandError, StorageError};

pub(super) fn map_storage_error(error: &StorageError) -> CommandAdmissionError {
    let code = match error.code() {
        crate::StorageErrorCode::Conflict | crate::StorageErrorCode::InvalidRequest => {
            CommandAdmissionErrorCode::InvalidRequest
        }
        crate::StorageErrorCode::Busy => CommandAdmissionErrorCode::Busy,
        crate::StorageErrorCode::CapacityExhausted => {
            CommandAdmissionErrorCode::StorageCapacityExhausted
        }
        _ => CommandAdmissionErrorCode::Unavailable,
    };
    CommandAdmissionError::new(code, error.detail())
}

pub(super) fn map_station_error(error: &StationCommandError) -> CommandAdmissionError {
    CommandAdmissionError::new(CommandAdmissionErrorCode::Unavailable, error.context())
}

pub(super) fn integrity_error(context: &str) -> CommandAdmissionError {
    CommandAdmissionError::new(CommandAdmissionErrorCode::Unavailable, context)
}
