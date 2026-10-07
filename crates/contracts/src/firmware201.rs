//! Reference-only OCPP 2.0.1 firmware updates (L01/L02) and value-free native/job evidence.
//!
//! Callers name a provider artifact; the trusted provider supplies the station location,
//! signing certificate and signature at the send boundary, never the request body.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const UPDATE_FIRMWARE_REFERENCE_SCHEMA_201: &str = "urn:uob:ocpp201:UpdateFirmwareReference:1";
const MAX_NATIVE_INTEGER: u32 = 2_147_483_647;

/// `UpdateFirmwareRequest` (§1.62) without location, certificate or signature. Whether the
/// station receives a secure (L01) or non-secure (L02) request is station policy.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateFirmwareReference201 {
    /// Native `requestId`; every status report for this update repeats it (L01.FR.10).
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub request_id: i32,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = "^[A-Za-z0-9_-][A-Za-z0-9._-]*$")
    )]
    pub artifact_reference: String,
    pub retrieve_date_time: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_date_time: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retry_interval: Option<u32>,
}

impl<'de> Deserialize<'de> for UpdateFirmwareReference201 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            request_id: i32,
            artifact_reference: String,
            retrieve_date_time: UtcTimestamp,
            #[serde(default)]
            install_date_time: Option<UtcTimestamp>,
            #[serde(default)]
            retries: Option<u32>,
            #[serde(default)]
            retry_interval: Option<u32>,
        }
        let value = Wire::deserialize(deserializer)?;
        if value
            .install_date_time
            .is_some_and(|install| install < value.retrieve_date_time)
        {
            return Err(serde::de::Error::custom(
                "firmware installation cannot precede retrieval",
            ));
        }
        if !crate::valid_firmware_artifact_reference(&value.artifact_reference) {
            return Err(serde::de::Error::custom(
                "invalid firmware artifact reference",
            ));
        }
        if value
            .retries
            .into_iter()
            .chain(value.retry_interval)
            .any(|count| count > MAX_NATIVE_INTEGER)
        {
            return Err(serde::de::Error::custom(
                "firmware retry value exceeds the native integer range",
            ));
        }
        Ok(Self {
            request_id: value.request_id,
            artifact_reference: value.artifact_reference,
            retrieve_date_time: value.retrieve_date_time,
            install_date_time: value.install_date_time,
            retries: value.retries,
            retry_interval: value.retry_interval,
        })
    }
}

/// Native `FirmwareStatusEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
pub enum FirmwareStatus201 {
    Downloaded,
    DownloadFailed,
    Downloading,
    DownloadScheduled,
    DownloadPaused,
    Idle,
    InstallationFailed,
    Installing,
    Installed,
    InstallRebooting,
    InstallScheduled,
    InstallVerificationFailed,
    InvalidSignature,
    SignatureVerified,
}

impl FirmwareStatus201 {
    /// Exact native enumeration value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Downloaded => "Downloaded",
            Self::DownloadFailed => "DownloadFailed",
            Self::Downloading => "Downloading",
            Self::DownloadScheduled => "DownloadScheduled",
            Self::DownloadPaused => "DownloadPaused",
            Self::Idle => "Idle",
            Self::InstallationFailed => "InstallationFailed",
            Self::Installing => "Installing",
            Self::Installed => "Installed",
            Self::InstallRebooting => "InstallRebooting",
            Self::InstallScheduled => "InstallScheduled",
            Self::InstallVerificationFailed => "InstallVerificationFailed",
            Self::InvalidSignature => "InvalidSignature",
            Self::SignatureVerified => "SignatureVerified",
        }
    }
}

/// Native `UpdateFirmwareStatusEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum UpdateFirmwareStatus201 {
    Accepted,
    Rejected,
    AcceptedCanceled,
    InvalidCertificate,
    RevokedCertificate,
}

impl UpdateFirmwareStatus201 {
    /// Whether the station took responsibility for the new firmware update.
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedCanceled)
    }
}

/// Sanitized OCPP-J 2.0.1 error code returned instead of a native result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum FirmwareCallError201 {
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

impl FirmwareCallError201 {
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
pub fn valid_firmware_reason_code_201(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Correlated native reply to the firmware request, never proof of installation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FirmwareReply201 {
    /// Native `UpdateFirmwareResponse.status` with its optional reason code.
    Status {
        status: UpdateFirmwareStatus201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(min = 1, max = 20), regex(pattern = "^[!-~]{1,20}$"))]
        reason_code: Option<String>,
    },
    /// Native CALLERROR; the station did not take the update.
    CallError { code: FirmwareCallError201 },
}

impl FirmwareReply201 {
    #[must_use]
    pub const fn accepted(&self) -> bool {
        match self {
            Self::Status { status, .. } => status.accepted(),
            Self::CallError { .. } => false,
        }
    }
    /// `AcceptedCanceled`: the station cancelled its earlier update for this one (L01.FR.24).
    #[must_use]
    pub const fn cancelled_previous(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: UpdateFirmwareStatus201::AcceptedCanceled,
                ..
            }
        )
    }
}

/// Bridge-side lifecycle of one firmware job. Only resolved states release the drain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareJobState201 {
    /// Durably admitted; no correlated reply yet.
    Pending,
    /// Possibly delivered without a correlated reply; never replayed.
    Uncertain,
    /// Station accepted; no progress reported yet.
    Accepted,
    DownloadScheduled,
    Downloading,
    DownloadPaused,
    Downloaded,
    SignatureVerified,
    InstallScheduled,
    InstallRebooting,
    Installing,
    /// Deadline elapsed without a terminal station fact; still blocks release drain.
    TimedOut,
    Installed,
    DownloadFailed,
    InstallationFailed,
    InstallVerificationFailed,
    InvalidSignature,
    /// Station refused the request (native status or CALLERROR).
    Rejected,
    /// Definitely never transmitted.
    NotSent,
    /// The station cancelled this job for a newer request (`AcceptedCanceled`, L01.FR.24).
    Cancelled,
    /// A newer request was accepted while this job was unresolved.
    Superseded,
    /// An `Idle` report: no firmware work in progress, outcome not reported.
    StationIdle,
}

impl FirmwareJobState201 {
    /// Physical firmware work is confirmed finished or never started.
    #[must_use]
    pub const fn resolved(self) -> bool {
        matches!(
            self,
            Self::Installed
                | Self::DownloadFailed
                | Self::InstallationFailed
                | Self::InstallVerificationFailed
                | Self::InvalidSignature
                | Self::Rejected
                | Self::NotSent
                | Self::Cancelled
                | Self::Superseded
                | Self::StationIdle
        )
    }
}

/// Public facts about the artifact sent; never the location, certificate or signature.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareArtifact201 {
    #[schemars(length(min = 1, max = 128))]
    pub artifact_reference: String,
    #[schemars(regex(pattern = "^[0-9a-f]{64}$"))]
    pub sha256: String,
    #[schemars(range(min = 1))]
    pub size_bytes: u64,
    /// A signing certificate and signature were sent (secure update, L01).
    pub signed: bool,
    /// Provider material is test-only; production composition refuses it.
    pub test_only: bool,
}

/// Durable job state, refreshed in the command result as native facts arrive.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareJob201 {
    pub revision: u64,
    pub state: FirmwareJobState201,
    /// Bridge deadline after which an unresolved job becomes `timed_out`.
    pub deadline: UtcTimestamp,
    pub observed_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<FirmwareStatus201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<UtcTimestamp>,
    /// Native notifications attributed to this job.
    pub notifications: u32,
    /// Attributed notifications that would regress or contradict the job; state unchanged.
    pub rejected_transitions: u32,
}

/// Does not carry the station location, certificate, signature or raw payload.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareResult201 {
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    pub request_id: i32,
    /// Secure update with signing certificate and signature (L01) or non-secure (L02).
    pub secure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<FirmwareArtifact201>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<FirmwareReply201>,
    pub job: FirmwareJob201,
}

impl FirmwareResult201 {
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.reply.as_ref().is_some_and(FirmwareReply201::accepted)
    }
}
