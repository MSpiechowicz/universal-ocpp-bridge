//! Strict native request parsing: OCPP 1.6 Edition 2 §6.25 (`GetDiagnostics`) and Security
//! Whitepaper Ed4 `GetLog` (use case N01).
use serde::Deserialize;
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

const REMOTE_LOCATION_LIMIT: usize = 512;

/// Which native family carried the request; `GetLog` also names its log type.
#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, Eq, PartialEq)]
pub enum LogKind {
    /// Legacy `GetDiagnostics`.
    Diagnostics,
    /// `GetLog` with `logType = DiagnosticsLog` (N01.FR.04).
    DiagnosticsLog,
    /// `GetLog` with `logType = SecurityLog` (N01.FR.03).
    SecurityLog,
}

impl LogKind {
    #[must_use]
    pub const fn legacy(self) -> bool {
        matches!(self, Self::Diagnostics)
    }
}

/// One validated native log request, independent of the CALL action that carried it.
pub(crate) struct LogRequest {
    pub(crate) kind: LogKind,
    pub(crate) request_id: Option<i32>,
    pub(crate) location: String,
    pub(crate) retries: u32,
    pub(crate) retry_interval: u32,
    pub(crate) oldest: Option<OffsetDateTime>,
    pub(crate) latest: Option<OffsetDateTime>,
}

impl Drop for LogRequest {
    fn drop(&mut self) {
        self.location.zeroize();
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Diagnostics {
    location: String,
    #[serde(default, deserialize_with = "present")]
    retries: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    retry_interval: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    start_time: Option<String>,
    #[serde(default, deserialize_with = "present")]
    stop_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetLog {
    log: LogParameters,
    log_type: String,
    request_id: i64,
    #[serde(default, deserialize_with = "present")]
    retries: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    retry_interval: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LogParameters {
    remote_location: String,
    #[serde(default, deserialize_with = "present")]
    oldest_timestamp: Option<String>,
    #[serde(default, deserialize_with = "present")]
    latest_timestamp: Option<String>,
}

/// Native CALLERROR code for a request that cannot be represented by the pinned schema.
pub(crate) type CallError = &'static str;

/// # Errors
/// Returns `FormationViolation` for an unparseable shape and `PropertyConstraintViolation` for
/// values outside the native range.
pub(crate) fn diagnostics(payload: &Value) -> Result<LogRequest, CallError> {
    let mut request = Diagnostics::deserialize(payload).map_err(|_| "FormationViolation")?;
    let result = (|| {
        let oldest = request.start_time.as_deref().map(timestamp).transpose()?;
        let latest = request.stop_time.as_deref().map(timestamp).transpose()?;
        window(oldest, latest)?;
        Ok(LogRequest {
            kind: LogKind::Diagnostics,
            request_id: None,
            location: location(&request.location, usize::MAX)?,
            retries: count(request.retries)?,
            retry_interval: count(request.retry_interval)?,
            oldest,
            latest,
        })
    })();
    request.location.zeroize();
    result
}

/// # Errors
/// As [`diagnostics`], plus the whitepaper `LogEnumType` and `remoteLocation` length.
pub(crate) fn get_log(payload: &Value) -> Result<LogRequest, CallError> {
    let mut request = GetLog::deserialize(payload).map_err(|_| "FormationViolation")?;
    let result = (|| {
        let kind = match request.log_type.as_str() {
            "DiagnosticsLog" => LogKind::DiagnosticsLog,
            "SecurityLog" => LogKind::SecurityLog,
            _ => return Err("PropertyConstraintViolation"),
        };
        let oldest = request
            .log
            .oldest_timestamp
            .as_deref()
            .map(timestamp)
            .transpose()?;
        let latest = request
            .log
            .latest_timestamp
            .as_deref()
            .map(timestamp)
            .transpose()?;
        window(oldest, latest)?;
        Ok(LogRequest {
            kind,
            request_id: Some(
                i32::try_from(request.request_id).map_err(|_| "PropertyConstraintViolation")?,
            ),
            location: location(&request.log.remote_location, REMOTE_LOCATION_LIMIT)?,
            retries: count(request.retries)?,
            retry_interval: count(request.retry_interval)?,
            oldest,
            latest,
        })
    })();
    request.log.remote_location.zeroize();
    result
}

/// A non-empty absolute URI (`anyURI` / `remoteLocation`).
fn location(value: &str, limit: usize) -> Result<String, CallError> {
    if value.is_empty() || value.len() > limit || url::Url::parse(value).is_err() {
        return Err("PropertyConstraintViolation");
    }
    Ok(value.to_owned())
}

fn window(oldest: Option<OffsetDateTime>, latest: Option<OffsetDateTime>) -> Result<(), CallError> {
    match (oldest, latest) {
        (Some(oldest), Some(latest)) if latest < oldest => Err("PropertyConstraintViolation"),
        _ => Ok(()),
    }
}

fn timestamp(value: &str) -> Result<OffsetDateTime, CallError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| "PropertyConstraintViolation")
}

fn count(value: Option<i64>) -> Result<u32, CallError> {
    value.map_or(Ok(0), |value| {
        u32::try_from(value)
            .ok()
            .filter(|value| i32::try_from(*value).is_ok())
            .ok_or("PropertyConstraintViolation")
    })
}

/// Rejects an explicit JSON `null` for an optional native field instead of treating it as absent.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
