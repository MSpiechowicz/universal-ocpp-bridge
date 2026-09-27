use uob_application::{DatabaseError, DatabaseErrorCode, DatabaseRetryClassification};

pub(crate) fn invalid(context: &'static str) -> DatabaseError {
    DatabaseError::new(
        DatabaseErrorCode::InvalidConfiguration,
        DatabaseRetryClassification::Permanent,
        context,
    )
}

pub(crate) fn unavailable(context: &'static str) -> DatabaseError {
    DatabaseError::new(
        DatabaseErrorCode::ConnectionUnavailable,
        DatabaseRetryClassification::Retryable,
        context,
    )
}

pub(crate) fn uncertain(context: &'static str) -> DatabaseError {
    DatabaseError::new(
        DatabaseErrorCode::ConnectionUnavailable,
        DatabaseRetryClassification::Uncertain,
        context,
    )
}

pub(crate) fn shutdown() -> DatabaseError {
    DatabaseError::new(
        DatabaseErrorCode::ShutdownDeadlineExceeded,
        DatabaseRetryClassification::Retryable,
        "postgres.shutdown.deadline",
    )
}
