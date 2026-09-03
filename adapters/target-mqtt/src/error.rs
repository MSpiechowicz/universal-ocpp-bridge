use rumqttc::{ConnectReturnCode, ConnectionError};
use tokio::task::JoinError;
use uob_application::{
    DeliveryOutcome, ErrorRetryClassification, TargetError, TargetErrorCode, TargetPortError,
};

use crate::mapping::MappingError;

pub(crate) fn check_report_task(
    result: Option<&Result<Result<(), TargetPortError>, JoinError>>,
) -> Result<(), TargetError> {
    match result {
        Some(Ok(Ok(()))) => Ok(()),
        _ => Err(TargetError::new(
            TargetErrorCode::ConnectionUnavailable,
            ErrorRetryClassification::Retryable,
            "mqtt.delivery_report_unavailable",
        )),
    }
}

pub(crate) fn permanent_refusal(error: &ConnectionError) -> bool {
    matches!(
        error,
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::RefusedProtocolVersion
                | ConnectReturnCode::BadClientId
                | ConnectReturnCode::BadUserNamePassword
                | ConnectReturnCode::NotAuthorized
        )
    )
}

pub(crate) fn permanent_mapping(error: MappingError) -> DeliveryOutcome {
    DeliveryOutcome::PermanentFailure {
        reason: error.reason().to_owned(),
    }
}

pub(crate) fn permanent_configuration(context: &'static str) -> TargetError {
    TargetError::new(
        TargetErrorCode::InvalidConfiguration,
        ErrorRetryClassification::Permanent,
        context,
    )
}

pub(crate) fn permanent_connection(context: &'static str) -> TargetError {
    TargetError::new(
        TargetErrorCode::ConnectionUnavailable,
        ErrorRetryClassification::Permanent,
        context,
    )
}

pub(crate) fn permanent_data(context: &'static str) -> TargetError {
    TargetError::new(
        TargetErrorCode::InvalidData,
        ErrorRetryClassification::Permanent,
        context,
    )
}
