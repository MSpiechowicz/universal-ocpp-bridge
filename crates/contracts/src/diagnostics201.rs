//! Destination-free OCPP 2.0.1 log retrieval (N01) with value-free native and job evidence.
//!
//! Callers never supply an upload location. The trusted artifact provider opens a bounded
//! destination at the send boundary; neither its location nor its identity appears here.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GET_LOG_REFERENCE_SCHEMA_201: &str = "urn:uob:ocpp201:GetLogReference:1";
const MAX_NATIVE_INTEGER: u32 = 2_147_483_647;

/// Native `LogEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
pub enum LogType201 {
    DiagnosticsLog,
    SecurityLog,
}

/// `GetLogRequest` (§1.28) without `log.remoteLocation`.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetLogReference201 {
    pub log_type: LogType201,
    /// Native `requestId`; every status report for this upload repeats it (N01.FR.07).
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub request_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_timestamp: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_timestamp: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retry_interval: Option<u32>,
}

impl<'de> Deserialize<'de> for GetLogReference201 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            log_type: LogType201,
            request_id: i32,
            #[serde(default)]
            oldest_timestamp: Option<UtcTimestamp>,
            #[serde(default)]
            latest_timestamp: Option<UtcTimestamp>,
            #[serde(default)]
            retries: Option<u32>,
            #[serde(default)]
            retry_interval: Option<u32>,
        }
        let value = Wire::deserialize(deserializer)?;
        if let (Some(oldest), Some(latest)) = (value.oldest_timestamp, value.latest_timestamp)
            && latest < oldest
        {
            return Err(serde::de::Error::custom("log window ends before it starts"));
        }
        if value
            .retries
            .into_iter()
            .chain(value.retry_interval)
            .any(|count| count > MAX_NATIVE_INTEGER)
        {
            return Err(serde::de::Error::custom(
                "log retry value exceeds the native integer range",
            ));
        }
        Ok(Self {
            log_type: value.log_type,
            request_id: value.request_id,
            oldest_timestamp: value.oldest_timestamp,
            latest_timestamp: value.latest_timestamp,
            retries: value.retries,
            retry_interval: value.retry_interval,
        })
    }
}

/// Native `UploadLogStatusEnumType` of `LogStatusNotificationRequest`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
pub enum LogUploadStatus201 {
    BadMessage,
    Idle,
    NotSupportedOperation,
    PermissionDenied,
    Uploaded,
    UploadFailure,
    Uploading,
    AcceptedCanceled,
}

impl LogUploadStatus201 {
    /// Exact native enumeration value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadMessage => "BadMessage",
            Self::Idle => "Idle",
            Self::NotSupportedOperation => "NotSupportedOperation",
            Self::PermissionDenied => "PermissionDenied",
            Self::Uploaded => "Uploaded",
            Self::UploadFailure => "UploadFailure",
            Self::Uploading => "Uploading",
            Self::AcceptedCanceled => "AcceptedCanceled",
        }
    }
}

/// Native `LogStatusEnumType` of `GetLogResponse`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum GetLogStatus201 {
    Accepted,
    Rejected,
    AcceptedCanceled,
}

impl GetLogStatus201 {
    /// Whether the station took responsibility for the upload.
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedCanceled)
    }
}

/// Sanitized OCPP-J 2.0.1 error code returned instead of a native result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DiagnosticsCallError201 {
    FormatViolation,
    GenericError,
    InternalError,
    MessageTypeNotSupported,
    NotImplemented,
    NotSupported,
    OccurrenceConstraintViolation,
    PropertyConstraintViolation,
    ProtocolError,
    RpcFrameworkError,
    SecurityError,
    TypeConstraintViolation,
}

impl DiagnosticsCallError201 {
    /// Maps an already sanitized remote code; unknown text becomes `GenericError`.
    #[must_use]
    pub fn from_code(code: &str) -> Self {
        match code {
            "FormatViolation" => Self::FormatViolation,
            "InternalError" => Self::InternalError,
            "MessageTypeNotSupported" => Self::MessageTypeNotSupported,
            "NotImplemented" => Self::NotImplemented,
            "NotSupported" => Self::NotSupported,
            "OccurrenceConstraintViolation" => Self::OccurrenceConstraintViolation,
            "PropertyConstraintViolation" => Self::PropertyConstraintViolation,
            "ProtocolError" => Self::ProtocolError,
            "RpcFrameworkError" => Self::RpcFrameworkError,
            "SecurityError" => Self::SecurityError,
            "TypeConstraintViolation" => Self::TypeConstraintViolation,
            _ => Self::GenericError,
        }
    }
}

/// Native `statusInfo.reasonCode`: 1–20 printable ASCII characters. `additionalInfo` is free
/// station text and is not retained.
#[must_use]
pub fn valid_log_reason_code_201(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Correlated native reply, never proof that a file was uploaded.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticsReply201 {
    /// Native `GetLogResponse` with its optional file name and reason code.
    Status {
        status: GetLogStatus201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 255))]
        file_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(min = 1, max = 20), regex(pattern = "^[!-~]{1,20}$"))]
        reason_code: Option<String>,
    },
    /// Native CALLERROR; the station did not take the upload.
    CallError { code: DiagnosticsCallError201 },
}

impl DiagnosticsReply201 {
    #[must_use]
    pub const fn accepted(&self) -> bool {
        match self {
            Self::Status { status, .. } => status.accepted(),
            Self::CallError { .. } => false,
        }
    }
    /// `AcceptedCanceled`: the station cancelled its earlier upload for this one (N01.FR.12).
    #[must_use]
    pub const fn cancelled_previous(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: GetLogStatus201::AcceptedCanceled,
                ..
            }
        )
    }
}

/// Bridge-side lifecycle of one log upload job. Only resolved states release the drain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsJobState201 {
    /// Durably admitted; no correlated reply yet.
    Pending,
    /// Possibly delivered without a correlated reply; never replayed.
    Uncertain,
    /// Station accepted; no progress reported yet.
    Accepted,
    Uploading,
    /// Deadline elapsed without a terminal station fact; still blocks release drain.
    TimedOut,
    /// Station reported `Uploaded` and the provider holds the stored file.
    Uploaded,
    /// Station reported `Uploaded` but the provider holds no complete file for the job.
    UploadUnconfirmed,
    /// Native `UploadFailure`.
    UploadFailed,
    BadMessage,
    NotSupportedOperation,
    PermissionDenied,
    /// Station refused the request (native status or CALLERROR).
    Rejected,
    /// Definitely never transmitted.
    NotSent,
    /// The station cancelled this upload for a newer request (N01.FR.12/20).
    Cancelled,
    /// A newer request was accepted while this job was unresolved.
    Superseded,
    /// A triggered `Idle` report: no upload in progress, outcome not reported (N01.FR.13).
    StationIdle,
}

impl DiagnosticsJobState201 {
    /// The station's upload work is confirmed finished or never started.
    #[must_use]
    pub const fn resolved(self) -> bool {
        !matches!(
            self,
            Self::Pending | Self::Uncertain | Self::Accepted | Self::Uploading | Self::TimedOut
        )
    }
}

/// Public facts about the destination offered; never its location or identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsDestination201 {
    pub log_type: LogType201,
    /// Effective byte cap the provider enforces.
    #[schemars(range(min = 1))]
    pub maximum_bytes: u64,
    /// Provider material is test-only; production composition refuses it.
    pub test_only: bool,
}

/// What the provider stored for this job, observed by the bridge, not reported by the station.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsUpload201 {
    #[schemars(regex(pattern = "^[0-9a-f]{64}$"))]
    pub sha256: String,
    pub size_bytes: u64,
}

/// Durable job state, refreshed in the command result as native facts arrive.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsJob201 {
    pub revision: u64,
    pub state: DiagnosticsJobState201,
    /// Bridge deadline after which an unresolved job becomes `timed_out`.
    pub deadline: UtcTimestamp,
    pub observed_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<LogUploadStatus201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<UtcTimestamp>,
    /// Native notifications attributed to this job, including late ones that change nothing.
    pub notifications: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<DiagnosticsUpload201>,
}

/// Does not carry the upload location, its identity, the log content or raw payloads.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsResult201 {
    pub log_type: LogType201,
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub request_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<DiagnosticsDestination201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<DiagnosticsReply201>,
    pub job: DiagnosticsJob201,
}

impl DiagnosticsResult201 {
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.reply
            .as_ref()
            .is_some_and(DiagnosticsReply201::accepted)
    }
}
