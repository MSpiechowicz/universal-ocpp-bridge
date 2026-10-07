//! Destination-free OCPP 1.6 diagnostics and Security Whitepaper log retrieval, with value-free
//! native and job evidence.
//!
//! Callers never supply an upload location. The trusted artifact provider opens a bounded
//! destination at the send boundary; neither its location nor its identity appears here.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GET_DIAGNOSTICS_REFERENCE_SCHEMA_16: &str = "urn:uob:ocpp16:GetDiagnosticsReference:1";
pub const GET_LOG_REFERENCE_SCHEMA_16: &str = "urn:uob:ocpp16:GetLogReference:1";
/// Longest native file name (`CiString255Type`, `GetLog` `filename` maxLength 255).
pub const MAX_LOG_FILE_NAME_16: usize = 255;
const MAX_NATIVE_INTEGER: u32 = 2_147_483_647;

/// OCPP 1.6 `GetDiagnostics` (§6.25) without the caller-supplied `location`.
#[derive(Clone, Debug, Default, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetDiagnosticsReference16 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_time: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retry_interval: Option<u32>,
}

/// Security Whitepaper `LogEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
pub enum LogType16 {
    DiagnosticsLog,
    SecurityLog,
}

/// Security Whitepaper Ed. 4 `GetLog` (N01) without `log.remoteLocation`.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetLogReference16 {
    pub log_type: LogType16,
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

fn native_count<E: serde::de::Error>(value: Option<u32>) -> Result<Option<u32>, E> {
    match value {
        Some(count) if count > MAX_NATIVE_INTEGER => Err(E::custom(
            "log retry value exceeds the native integer range",
        )),
        value => Ok(value),
    }
}

fn window<E: serde::de::Error>(
    oldest: Option<UtcTimestamp>,
    latest: Option<UtcTimestamp>,
) -> Result<(), E> {
    if let (Some(oldest), Some(latest)) = (oldest, latest)
        && latest < oldest
    {
        return Err(E::custom("log window ends before it starts"));
    }
    Ok(())
}

impl<'de> Deserialize<'de> for GetDiagnosticsReference16 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            #[serde(default)]
            start_time: Option<UtcTimestamp>,
            #[serde(default)]
            stop_time: Option<UtcTimestamp>,
            #[serde(default)]
            retries: Option<u32>,
            #[serde(default)]
            retry_interval: Option<u32>,
        }
        let value = Wire::deserialize(deserializer)?;
        window(value.start_time, value.stop_time)?;
        Ok(Self {
            start_time: value.start_time,
            stop_time: value.stop_time,
            retries: native_count(value.retries)?,
            retry_interval: native_count(value.retry_interval)?,
        })
    }
}

impl<'de> Deserialize<'de> for GetLogReference16 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            log_type: LogType16,
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
        window(value.oldest_timestamp, value.latest_timestamp)?;
        Ok(Self {
            log_type: value.log_type,
            request_id: value.request_id,
            oldest_timestamp: value.oldest_timestamp,
            latest_timestamp: value.latest_timestamp,
            retries: native_count(value.retries)?,
            retry_interval: native_count(value.retry_interval)?,
        })
    }
}

/// Native upload status from either notification. `DiagnosticsStatusNotification` (§7.24)
/// uses only `Idle`, `Uploaded`, `UploadFailed` and `Uploading`; `LogStatusNotification`
/// (`UploadLogStatusEnumType`) uses every value except `UploadFailed`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
pub enum LogUploadStatus16 {
    BadMessage,
    Idle,
    NotSupportedOperation,
    PermissionDenied,
    Uploaded,
    UploadFailed,
    UploadFailure,
    Uploading,
}

impl LogUploadStatus16 {
    /// Values defined for `DiagnosticsStatusNotification.req`.
    #[must_use]
    pub const fn diagnostics(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Uploaded | Self::UploadFailed | Self::Uploading
        )
    }
    /// Values defined for `LogStatusNotification.req`.
    #[must_use]
    pub const fn log(self) -> bool {
        !matches!(self, Self::UploadFailed)
    }
}

/// `GetLog.conf` status (`LogStatusEnumType`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum GetLogStatus16 {
    Accepted,
    Rejected,
    AcceptedCanceled,
}

impl GetLogStatus16 {
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedCanceled)
    }
}

/// Sanitized OCPP-J error code returned instead of a native result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum DiagnosticsCallError16 {
    FormationViolation,
    GenericError,
    InternalError,
    NotImplemented,
    NotSupported,
    OccurrenceConstraintViolation,
    PropertyConstraintViolation,
    ProtocolError,
    SecurityError,
    TypeConstraintViolation,
}

impl DiagnosticsCallError16 {
    /// Maps an already sanitized remote code; unknown text becomes `GenericError`.
    #[must_use]
    pub fn from_code(code: &str) -> Self {
        match code {
            "FormationViolation" => Self::FormationViolation,
            "InternalError" => Self::InternalError,
            "NotImplemented" => Self::NotImplemented,
            "NotSupported" => Self::NotSupported,
            "OccurrenceConstraintViolation" => Self::OccurrenceConstraintViolation,
            "PropertyConstraintViolation" => Self::PropertyConstraintViolation,
            "ProtocolError" => Self::ProtocolError,
            "SecurityError" => Self::SecurityError,
            "TypeConstraintViolation" => Self::TypeConstraintViolation,
            _ => Self::GenericError,
        }
    }
}

/// Whether a station-reported file name is a printable-ASCII `CiString255Type`.
#[must_use]
pub fn valid_log_file_name(value: &str) -> bool {
    value.len() <= MAX_LOG_FILE_NAME_16 && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

/// Correlated native reply, never proof that a file was uploaded.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticsReply16 {
    /// `GetDiagnostics.conf`; no file name means no diagnostics are available (§5.9).
    Diagnostics {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 255))]
        file_name: Option<String>,
    },
    /// `GetLog.conf`.
    Log {
        status: GetLogStatus16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 255))]
        file_name: Option<String>,
    },
    /// Native CALLERROR.
    CallError { code: DiagnosticsCallError16 },
}

impl DiagnosticsReply16 {
    /// Whether the station took responsibility for an upload.
    #[must_use]
    pub const fn accepted(&self) -> bool {
        match self {
            Self::Diagnostics { file_name } => file_name.is_some(),
            Self::Log { status, .. } => status.accepted(),
            Self::CallError { .. } => false,
        }
    }
}

/// Bridge-side lifecycle of one log upload job. Only resolved states release the drain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsJobState16 {
    /// Durably admitted; no correlated reply yet.
    Pending,
    /// Possibly delivered without a correlated reply; never replayed.
    Uncertain,
    /// Station accepted and named a file; no progress reported yet.
    Accepted,
    Uploading,
    /// Deadline elapsed without a terminal station fact; still blocks release drain.
    TimedOut,
    /// Station reported `Uploaded` and the provider holds the stored file.
    Uploaded,
    /// Station reported `Uploaded` but the provider holds no complete file for the job.
    UploadUnconfirmed,
    /// `UploadFailed` (diagnostics) or `UploadFailure` (log).
    UploadFailed,
    BadMessage,
    NotSupportedOperation,
    PermissionDenied,
    /// `GetDiagnostics.conf` without a file name: nothing to upload (§5.9).
    NoLogAvailable,
    /// Station refused the request (native status or CALLERROR).
    Rejected,
    /// Definitely never transmitted.
    NotSent,
    /// The station cancelled this upload for a newer request (N01.FR.11).
    Cancelled,
    /// A newer request was accepted while this job was unresolved.
    Superseded,
    /// A triggered `Idle` report: no upload in progress, outcome not reported.
    StationIdle,
}

impl DiagnosticsJobState16 {
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
pub struct DiagnosticsDestination16 {
    pub log_type: LogType16,
    /// Effective byte cap the provider enforces.
    #[schemars(range(min = 1))]
    pub maximum_bytes: u64,
    /// Provider material is test-only; production composition refuses it.
    pub test_only: bool,
}

/// What the provider stored for this job, observed by the bridge, not reported by the station.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsUpload16 {
    #[schemars(regex(pattern = "^[0-9a-f]{64}$"))]
    pub sha256: String,
    pub size_bytes: u64,
}

/// Durable job state, refreshed in the command result as native facts arrive.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsJob16 {
    pub revision: u64,
    pub state: DiagnosticsJobState16,
    /// Bridge deadline after which an unresolved job becomes `timed_out`.
    pub deadline: UtcTimestamp,
    pub observed_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<LogUploadStatus16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<UtcTimestamp>,
    /// Native notifications attributed to this job.
    pub notifications: u32,
    /// Attributed notifications that would regress or contradict the job; state unchanged.
    pub rejected_transitions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<DiagnosticsUpload16>,
}

/// Does not carry the upload location, its identity, the log content or raw payloads.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum DiagnosticsResult16 {
    GetDiagnostics {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<DiagnosticsDestination16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<DiagnosticsReply16>,
        job: DiagnosticsJob16,
    },
    GetLog {
        log_type: LogType16,
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        request_id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<DiagnosticsDestination16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<DiagnosticsReply16>,
        job: DiagnosticsJob16,
    },
}

impl DiagnosticsResult16 {
    #[must_use]
    pub const fn reply(&self) -> Option<&DiagnosticsReply16> {
        match self {
            Self::GetDiagnostics { reply, .. } | Self::GetLog { reply, .. } => reply.as_ref(),
        }
    }
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.reply().is_some_and(DiagnosticsReply16::accepted)
    }
    #[must_use]
    pub const fn job(&self) -> &DiagnosticsJob16 {
        match self {
            Self::GetDiagnostics { job, .. } | Self::GetLog { job, .. } => job,
        }
    }
    pub fn job_mut(&mut self) -> &mut DiagnosticsJob16 {
        match self {
            Self::GetDiagnostics { job, .. } | Self::GetLog { job, .. } => job,
        }
    }
    #[must_use]
    pub const fn destination(&self) -> Option<&DiagnosticsDestination16> {
        match self {
            Self::GetDiagnostics { destination, .. } | Self::GetLog { destination, .. } => {
                destination.as_ref()
            }
        }
    }
}
