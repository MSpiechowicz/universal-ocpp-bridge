//! Strict native `GetLogRequest` parsing against the pinned OCPP 2.0.1 schema (use case N01).
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/GetLogRequest.json"
    ))
    .expect("pinned schema")
});

/// `LogEnumType`: which log file the station uploads (N01.FR.03 and N01.FR.04).
#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, Eq, PartialEq)]
pub enum LogKind201 {
    DiagnosticsLog,
    SecurityLog,
}

/// One validated native request; the location never appears in debug output.
pub(crate) struct LogRequest201 {
    pub(crate) kind: LogKind201,
    pub(crate) request_id: i32,
    pub(crate) location: String,
    pub(crate) retries: u32,
    pub(crate) retry_interval: u32,
    pub(crate) oldest: Option<OffsetDateTime>,
    pub(crate) latest: Option<OffsetDateTime>,
}

impl Drop for LogRequest201 {
    fn drop(&mut self) {
        self.location.zeroize();
    }
}

// The pinned schema already refused unknown fields; vendor customData is ignored here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Native {
    log: NativeLog,
    log_type: String,
    request_id: i64,
    retries: Option<i64>,
    retry_interval: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeLog {
    remote_location: String,
    oldest_timestamp: Option<String>,
    latest_timestamp: Option<String>,
}

/// # Errors
/// `FormationViolation` for a payload outside the pinned schema and
/// `PropertyConstraintViolation` for values outside the native integer, time or URI range.
pub(crate) fn parse(payload: &Value) -> Result<LogRequest201, &'static str> {
    // An explicit null is never an absent optional field.
    if payload
        .pointer("/log")
        .and_then(Value::as_object)
        .is_some_and(|fields| fields.values().any(Value::is_null))
        || payload
            .as_object()
            .is_some_and(|fields| fields.values().any(Value::is_null))
        || !crate::local_authorization201::validation::valid_native(&SCHEMA, payload)
    {
        return Err("FormationViolation");
    }
    let mut native = Native::deserialize(payload).map_err(|_| "FormationViolation")?;
    let result = (|| {
        let kind = match native.log_type.as_str() {
            "DiagnosticsLog" => LogKind201::DiagnosticsLog,
            "SecurityLog" => LogKind201::SecurityLog,
            _ => return Err("PropertyConstraintViolation"),
        };
        let oldest = native
            .log
            .oldest_timestamp
            .as_deref()
            .map(timestamp)
            .transpose()?;
        let latest = native
            .log
            .latest_timestamp
            .as_deref()
            .map(timestamp)
            .transpose()?;
        if oldest
            .zip(latest)
            .is_some_and(|(oldest, latest)| latest < oldest)
        {
            return Err("PropertyConstraintViolation");
        }
        // A non-empty absolute URI (`remoteLocation`, maxLength 512 in the schema).
        if native.log.remote_location.is_empty()
            || url::Url::parse(&native.log.remote_location).is_err()
        {
            return Err("PropertyConstraintViolation");
        }
        Ok(LogRequest201 {
            kind,
            request_id: i32::try_from(native.request_id)
                .map_err(|_| "PropertyConstraintViolation")?,
            location: native.log.remote_location.clone(),
            retries: count(native.retries)?,
            retry_interval: count(native.retry_interval)?,
            oldest,
            latest,
        })
    })();
    native.log.remote_location.zeroize();
    result
}

fn timestamp(value: &str) -> Result<OffsetDateTime, &'static str> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| "PropertyConstraintViolation")
}

fn count(value: Option<i64>) -> Result<u32, &'static str> {
    value.map_or(Ok(0), |value| {
        u32::try_from(value)
            .ok()
            .filter(|value| i32::try_from(*value).is_ok())
            .ok_or("PropertyConstraintViolation")
    })
}
