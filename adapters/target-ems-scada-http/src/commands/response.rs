use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use uob_application::CommandAdmissionErrorCode;
use uob_contracts::{CommandErrorCode, CommandLifecycle, CommandResult};

use super::AcceptedCommand;
use crate::error::IntegrationErrorCode;

pub(super) fn admission_error(code: CommandAdmissionErrorCode) -> IntegrationErrorCode {
    match code {
        CommandAdmissionErrorCode::Unauthorized => IntegrationErrorCode::PermissionDenied,
        CommandAdmissionErrorCode::Expired => IntegrationErrorCode::Expired,
        CommandAdmissionErrorCode::Unsupported => IntegrationErrorCode::CommandUnsupported,
        CommandAdmissionErrorCode::PolicyRejected => IntegrationErrorCode::CommandPolicyRejected,
        CommandAdmissionErrorCode::Busy => IntegrationErrorCode::CommandBusy,
        CommandAdmissionErrorCode::StorageCapacityExhausted
        | CommandAdmissionErrorCode::Unavailable => IntegrationErrorCode::SourceUnavailable,
        CommandAdmissionErrorCode::InvalidRequest => IntegrationErrorCode::RequestConflict,
    }
}

pub(super) fn command(result: CommandResult) -> Response {
    if let CommandLifecycle::Rejected { error } = &result.lifecycle {
        let status = match error.code {
            CommandErrorCode::Unauthorized => StatusCode::FORBIDDEN,
            CommandErrorCode::Expired => StatusCode::GONE,
            CommandErrorCode::UnsupportedOperation => StatusCode::UNPROCESSABLE_ENTITY,
            CommandErrorCode::StationDisconnected => StatusCode::CONFLICT,
            CommandErrorCode::InvalidParameters
            | CommandErrorCode::PolicyRejected
            | CommandErrorCode::ProtocolRejected => StatusCode::BAD_REQUEST,
        };
        return (status, Json(result)).into_response();
    }
    let request_id = result.return_route.request_id.as_str().to_owned();
    let status_url = format!("/bridge/v1/commands/{}", path_segment(&request_id));
    (
        StatusCode::ACCEPTED,
        Json(AcceptedCommand {
            request_id,
            status_url,
            result,
        }),
    )
        .into_response()
}

/// Percent-encode request identities as one UTF-8 path segment, never as URL syntax.
fn path_segment(value: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("write to string");
        }
    }
    encoded
}
