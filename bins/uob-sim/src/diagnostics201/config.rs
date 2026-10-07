//! Operator-facing OCPP 2.0.1 station log options and the fault knobs copied into the live model.
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAXIMUM_BYTES_LIMIT: u64 = 64 * 1024 * 1024;

/// What a station does when a new `GetLog` arrives while an upload is ongoing (N01.FR.12).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LogCancelPolicy201 {
    /// Cancel the ongoing upload and reply `AcceptedCanceled` (N01.FR.12/20).
    #[default]
    Cancel,
    /// Keep the ongoing upload and reply `Rejected`.
    Reject,
}

/// Failure status an upload reports once every attempt failed (N01.FR.10).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub enum LogFailureStatus201 {
    #[default]
    UploadFailure,
    BadMessage,
    PermissionDenied,
    NotSupportedOperation,
}

impl LogFailureStatus201 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UploadFailure => "UploadFailure",
            Self::BadMessage => "BadMessage",
            Self::PermissionDenied => "PermissionDenied",
            Self::NotSupportedOperation => "NotSupportedOperation",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostics201Config {
    pub private_state_file: String,
    #[serde(default = "default_maximum_bytes")]
    pub maximum_bytes: u64,
    #[serde(default = "default_upload_timeout_ms")]
    pub upload_timeout_ms: u64,
    #[serde(default)]
    pub status_delay_ms: u64,
    #[serde(default = "default_diagnostics_bytes")]
    pub diagnostics_bytes: u64,
    #[serde(default = "default_security_log_bytes")]
    pub security_log_bytes: u64,
    #[serde(default)]
    pub upload_failures: u32,
    /// Answer every new request `Rejected` (N01.FR.05: the requested log is not available).
    #[serde(default)]
    pub reject_get_log: bool,
    #[serde(default)]
    pub failure_status: LogFailureStatus201,
    #[serde(default)]
    pub cancel_policy: LogCancelPolicy201,
}

impl Diagnostics201Config {
    /// # Errors
    /// Rejects bounds outside the simulator's supported ranges and failure knobs that cannot apply.
    pub fn validate(&self) -> Result<(), &'static str> {
        let content = |size: u64| size == 0 || size > self.maximum_bytes;
        if self.maximum_bytes == 0
            || self.maximum_bytes > MAXIMUM_BYTES_LIMIT
            || self.upload_timeout_ms == 0
            || self.upload_timeout_ms > 300_000
            || self.status_delay_ms > 30_000
            || self.upload_failures > 16
            || content(self.diagnostics_bytes)
            || content(self.security_log_bytes)
            || (self.reject_get_log && self.upload_failures > 0)
        {
            return Err("diagnostics_config_invalid");
        }
        Ok(())
    }
}

/// Fault and timing knobs copied from configuration.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) maximum_bytes: u64,
    pub(crate) upload_timeout: Duration,
    pub(crate) status_delay: Duration,
    pub(crate) diagnostics_bytes: u64,
    pub(crate) security_log_bytes: u64,
    pub(crate) upload_failures: u32,
    pub(crate) reject_get_log: bool,
    pub(crate) failure_status: LogFailureStatus201,
    pub(crate) cancel_policy: LogCancelPolicy201,
}

impl From<&Diagnostics201Config> for Settings {
    fn from(config: &Diagnostics201Config) -> Self {
        Self {
            maximum_bytes: config.maximum_bytes,
            upload_timeout: Duration::from_millis(config.upload_timeout_ms),
            status_delay: Duration::from_millis(config.status_delay_ms),
            diagnostics_bytes: config.diagnostics_bytes,
            security_log_bytes: config.security_log_bytes,
            upload_failures: config.upload_failures,
            reject_get_log: config.reject_get_log,
            failure_status: config.failure_status,
            cancel_policy: config.cancel_policy,
        }
    }
}

const fn default_maximum_bytes() -> u64 {
    1024 * 1024
}

const fn default_upload_timeout_ms() -> u64 {
    30_000
}

const fn default_diagnostics_bytes() -> u64 {
    4096
}

const fn default_security_log_bytes() -> u64 {
    2048
}
