use serde_json::{Value, json};
use uob_contracts::ProtocolEdition;

use crate::{OcppCallError, OcppErrorCode};

pub(super) enum Frame {
    Call {
        message_id: String,
        bytes: Vec<u8>,
    },
    Result {
        message_id: String,
        payload: Value,
    },
    Error {
        message_id: String,
        code: String,
        _description: String,
        details: Value,
    },
}

pub(super) struct FrameError {
    pub message_id: Option<String>,
    pub field_path: &'static str,
}

pub(super) fn decode(bytes: &[u8]) -> Result<Frame, FrameError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid(None, "/"))?;
    let items = value.as_array().ok_or_else(|| invalid(None, "/"))?;
    let message_id = items
        .get(1)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    match items.first().and_then(Value::as_u64) {
        Some(2) if items.len() == 4 => {
            let id = message_id.ok_or_else(|| invalid(None, "/1"))?;
            if items[2]
                .as_str()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(invalid(Some(id), "/2"));
            }
            if !items[3].is_object() {
                return Err(invalid(Some(id), "/3"));
            }
            Ok(Frame::Call {
                message_id: id,
                bytes: bytes.to_vec(),
            })
        }
        Some(3) if items.len() == 3 => {
            let id = message_id.ok_or_else(|| invalid(None, "/1"))?;
            if !items[2].is_object() {
                return Err(invalid(Some(id), "/2"));
            }
            Ok(Frame::Result {
                message_id: id,
                payload: items[2].clone(),
            })
        }
        Some(4) if items.len() == 5 => {
            let id = message_id.ok_or_else(|| invalid(None, "/1"))?;
            let code = items[2]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| invalid(Some(id.clone()), "/2"))?;
            let description = items[3]
                .as_str()
                .ok_or_else(|| invalid(Some(id.clone()), "/3"))?;
            if !items[4].is_object() {
                return Err(invalid(Some(id), "/4"));
            }
            Ok(Frame::Error {
                message_id: id,
                code: code.to_owned(),
                _description: description.to_owned(),
                details: items[4].clone(),
            })
        }
        _ => Err(invalid(message_id, "/0")),
    }
}

pub(super) fn call(message_id: &str, action: &str, payload: &Value) -> String {
    serde_json::to_string(&json!([2, message_id, action, payload]))
        .expect("OCPP CALL values are serializable")
}

pub(super) fn result(message_id: &str, payload: &Value) -> String {
    serde_json::to_string(&json!([3, message_id, payload]))
        .expect("OCPP CALLRESULT values are serializable")
}

pub(super) fn error(message_id: &str, error: OcppCallError) -> String {
    let details = error
        .field_path
        .map_or_else(|| json!({}), |path| json!({ "path": path }));
    serde_json::to_string(&json!([
        4,
        message_id,
        error.code.as_str(),
        error.description,
        details
    ]))
    .expect("OCPP CALLERROR values are serializable")
}

pub(super) const fn protocol_error(
    protocol: ProtocolEdition,
    description: &'static str,
    field_path: Option<&'static str>,
) -> OcppCallError {
    OcppCallError {
        protocol,
        code: OcppErrorCode::ProtocolError,
        description,
        field_path,
    }
}

pub(super) fn safe_remote_code(code: &str) -> &'static str {
    match code {
        "FormationViolation" => "FormationViolation",
        "InternalError" => "InternalError",
        "NotImplemented" => "NotImplemented",
        "NotSupported" => "NotSupported",
        "OccurrenceConstraintViolation" => "OccurrenceConstraintViolation",
        "PropertyConstraintViolation" => "PropertyConstraintViolation",
        "ProtocolError" => "ProtocolError",
        "SecurityError" => "SecurityError",
        "TypeConstraintViolation" => "TypeConstraintViolation",
        _ => "GenericError",
    }
}

pub(super) fn safe_field_path(details: &Value) -> Option<String> {
    details
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| {
            path.starts_with('/') && path.len() <= 256 && !path.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

const fn invalid(message_id: Option<String>, field_path: &'static str) -> FrameError {
    FrameError {
        message_id,
        field_path,
    }
}
