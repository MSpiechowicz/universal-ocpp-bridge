//! Reference-only OCPP 1.6 firmware updates and value-free native/job evidence.
//!
//! Callers name a provider artifact; the trusted provider supplies the station location,
//! signing certificate and signature at the send boundary, never the request body.
use crate::UtcTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const UPDATE_FIRMWARE_REFERENCE_SCHEMA_16: &str = "urn:uob:ocpp16:UpdateFirmwareReference:1";
pub const SIGNED_UPDATE_FIRMWARE_REFERENCE_SCHEMA_16: &str =
    "urn:uob:ocpp16:SignedUpdateFirmwareReference:1";
const MAX_NATIVE_INTEGER: u32 = 2_147_483_647;

/// Same syntax as the application artifact reference: 1–128 bytes of `[A-Za-z0-9._-]`
/// without a leading dot.
#[must_use]
pub fn valid_firmware_artifact_reference(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Legacy OCPP 1.6 `UpdateFirmware` (§6.55) without a caller-supplied location.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateFirmwareReference16 {
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = "^[A-Za-z0-9_-][A-Za-z0-9._-]*$")
    )]
    pub artifact_reference: String,
    pub retrieve_date: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 2_147_483_647))]
    pub retry_interval: Option<u32>,
}

/// Security Whitepaper Ed. 4 `SignedUpdateFirmware` (§5.21) without location or signature.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedUpdateFirmwareReference16 {
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

fn native_count<E: serde::de::Error>(value: Option<u32>) -> Result<Option<u32>, E> {
    match value {
        Some(count) if count > MAX_NATIVE_INTEGER => Err(E::custom(
            "firmware retry value exceeds the native integer range",
        )),
        value => Ok(value),
    }
}

fn reference<E: serde::de::Error>(value: String) -> Result<String, E> {
    if valid_firmware_artifact_reference(&value) {
        Ok(value)
    } else {
        Err(E::custom("invalid firmware artifact reference"))
    }
}

impl<'de> Deserialize<'de> for UpdateFirmwareReference16 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            artifact_reference: String,
            retrieve_date: UtcTimestamp,
            #[serde(default)]
            retries: Option<u32>,
            #[serde(default)]
            retry_interval: Option<u32>,
        }
        let value = Wire::deserialize(deserializer)?;
        Ok(Self {
            artifact_reference: reference(value.artifact_reference)?,
            retrieve_date: value.retrieve_date,
            retries: native_count(value.retries)?,
            retry_interval: native_count(value.retry_interval)?,
        })
    }
}

impl<'de> Deserialize<'de> for SignedUpdateFirmwareReference16 {
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
        Ok(Self {
            request_id: value.request_id,
            artifact_reference: reference(value.artifact_reference)?,
            retrieve_date_time: value.retrieve_date_time,
            install_date_time: value.install_date_time,
            retries: native_count(value.retries)?,
            retry_interval: native_count(value.retry_interval)?,
        })
    }
}

/// Native station status from either 1.6 notification. Legacy stations use only seven.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
pub enum FirmwareStatus16 {
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

impl FirmwareStatus16 {
    /// Values defined by OCPP 1.6 §7.25 for the original `FirmwareStatusNotification`.
    #[must_use]
    pub const fn legacy(self) -> bool {
        matches!(
            self,
            Self::Downloaded
                | Self::DownloadFailed
                | Self::Downloading
                | Self::Idle
                | Self::InstallationFailed
                | Self::Installing
                | Self::Installed
        )
    }
}

/// `SignedUpdateFirmware.conf` status (Whitepaper §6.18).
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SignedUpdateFirmwareStatus16 {
    Accepted,
    Rejected,
    AcceptedCanceled,
    InvalidCertificate,
    RevokedCertificate,
}

impl SignedUpdateFirmwareStatus16 {
    /// Whether the station took responsibility for the new firmware update.
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedCanceled)
    }
}

/// Sanitized OCPP-J error code returned instead of a native result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum FirmwareCallError16 {
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

impl FirmwareCallError16 {
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

/// Correlated native reply to the firmware request, never proof of installation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FirmwareReply16 {
    /// Empty legacy `UpdateFirmware.conf`; 1.6 defines no rejection status.
    Acknowledged,
    /// Native `SignedUpdateFirmware.conf` status.
    Status {
        status: SignedUpdateFirmwareStatus16,
    },
    /// Native CALLERROR, such as `NotSupported` from a signed-only station (L01.FR.20).
    CallError { code: FirmwareCallError16 },
}

impl FirmwareReply16 {
    #[must_use]
    pub const fn accepted(self) -> bool {
        match self {
            Self::Acknowledged => true,
            Self::Status { status } => status.accepted(),
            Self::CallError { .. } => false,
        }
    }
}

/// Bridge-side lifecycle of one firmware job. Only resolved states release the drain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareJobState16 {
    /// Durably admitted; no correlated reply yet.
    Pending,
    /// Possibly delivered without a correlated reply; never replayed.
    Uncertain,
    /// Station acknowledged or accepted; no progress reported yet.
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
    /// The station cancelled this job for a newer signed request (L01.FR.26).
    Cancelled,
    /// A newer request was accepted while this job was unresolved.
    Superseded,
    /// A triggered `Idle` report: no firmware work in progress, outcome not reported.
    StationIdle,
}

impl FirmwareJobState16 {
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
pub struct FirmwareArtifact16 {
    #[schemars(length(min = 1, max = 128))]
    pub artifact_reference: String,
    #[schemars(regex(pattern = "^[0-9a-f]{64}$"))]
    pub sha256: String,
    #[schemars(range(min = 1))]
    pub size_bytes: u64,
    pub signed: bool,
    /// Provider material is test-only; production composition refuses it.
    pub test_only: bool,
}

/// Durable job state, refreshed in the command result as native facts arrive.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareJob16 {
    pub revision: u64,
    pub state: FirmwareJobState16,
    /// Bridge deadline after which an unresolved job becomes `timed_out`.
    pub deadline: UtcTimestamp,
    pub observed_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<FirmwareStatus16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<UtcTimestamp>,
    /// Native notifications attributed to this job.
    pub notifications: u32,
    /// Attributed notifications that would regress or contradict the job; state unchanged.
    pub rejected_transitions: u32,
}

/// Does not carry the station location, certificate, signature or raw payload.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum FirmwareResult16 {
    UpdateFirmware {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<FirmwareArtifact16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<FirmwareReply16>,
        job: FirmwareJob16,
    },
    SignedUpdateFirmware {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        request_id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<FirmwareArtifact16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<FirmwareReply16>,
        job: FirmwareJob16,
    },
}

impl FirmwareResult16 {
    #[must_use]
    pub fn reply(&self) -> Option<FirmwareReply16> {
        match self {
            Self::UpdateFirmware { reply, .. } | Self::SignedUpdateFirmware { reply, .. } => *reply,
        }
    }
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.reply().is_some_and(FirmwareReply16::accepted)
    }
    #[must_use]
    pub const fn job(&self) -> &FirmwareJob16 {
        match self {
            Self::UpdateFirmware { job, .. } | Self::SignedUpdateFirmware { job, .. } => job,
        }
    }
    pub fn job_mut(&mut self) -> &mut FirmwareJob16 {
        match self {
            Self::UpdateFirmware { job, .. } | Self::SignedUpdateFirmware { job, .. } => job,
        }
    }
    #[must_use]
    pub const fn artifact(&self) -> Option<&FirmwareArtifact16> {
        match self {
            Self::UpdateFirmware { artifact, .. } | Self::SignedUpdateFirmware { artifact, .. } => {
                artifact.as_ref()
            }
        }
    }
}
