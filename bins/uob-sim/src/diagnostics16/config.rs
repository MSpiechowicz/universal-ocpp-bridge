//! Operator-facing station diagnostics options and the fault knobs copied into the live model.
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAXIMUM_BYTES_LIMIT: u64 = 64 * 1024 * 1024;

/// What a station does when a new `GetLog` arrives while an upload is ongoing (N01.FR.11).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LogCancelPolicy {
    /// Cancel the ongoing upload and reply `AcceptedCanceled`.
    #[default]
    Cancel,
    /// Keep the ongoing upload and reply `Rejected`.
    Reject,
}

/// Failure status a `GetLog` upload reports once every attempt failed (N01.FR.10).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub enum LogFailureStatus {
    #[default]
    UploadFailure,
    BadMessage,
    PermissionDenied,
    NotSupportedOperation,
}

impl LogFailureStatus {
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
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent operator-facing TOML key.
pub struct DiagnosticsConfig {
    pub private_state_file: String,
    /// OCPP 1.6 `GetDiagnostics` / `DiagnosticsStatusNotification`.
    #[serde(default = "enabled")]
    pub legacy: bool,
    /// Security Whitepaper `GetLog` / `LogStatusNotification`.
    #[serde(default)]
    pub security_log: bool,
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
    pub no_diagnostics: bool,
    #[serde(default)]
    pub upload_failures: u32,
    #[serde(default)]
    pub reject_get_log: bool,
    #[serde(default)]
    pub get_log_failure_status: LogFailureStatus,
    #[serde(default)]
    pub cancel_policy: LogCancelPolicy,
}

impl DiagnosticsConfig {
    /// # Errors
    /// Rejects bounds outside the simulator's supported ranges and family mismatches.
    pub fn validate(&self) -> Result<(), &'static str> {
        let content = |size: u64| size == 0 || size > self.maximum_bytes;
        if !(self.legacy || self.security_log)
            || self.maximum_bytes == 0
            || self.maximum_bytes > MAXIMUM_BYTES_LIMIT
            || self.upload_timeout_ms == 0
            || self.upload_timeout_ms > 300_000
            || self.status_delay_ms > 30_000
            || self.upload_failures > 16
            || content(self.diagnostics_bytes)
            || content(self.security_log_bytes)
            || (!self.legacy && self.no_diagnostics)
            || (!self.security_log
                && (self.reject_get_log
                    || self.get_log_failure_status != LogFailureStatus::UploadFailure))
        {
            return Err("diagnostics_config_invalid");
        }
        Ok(())
    }
}

/// Fault and timing knobs copied from configuration.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // Mirrors the independent configuration flags.
pub(crate) struct Settings {
    pub(crate) legacy: bool,
    pub(crate) security_log: bool,
    pub(crate) maximum_bytes: u64,
    pub(crate) upload_timeout: Duration,
    pub(crate) status_delay: Duration,
    pub(crate) diagnostics_bytes: u64,
    pub(crate) security_log_bytes: u64,
    pub(crate) no_diagnostics: bool,
    pub(crate) upload_failures: u32,
    pub(crate) reject_get_log: bool,
    pub(crate) failure_status: LogFailureStatus,
    pub(crate) cancel_policy: LogCancelPolicy,
}

impl From<&DiagnosticsConfig> for Settings {
    fn from(config: &DiagnosticsConfig) -> Self {
        Self {
            legacy: config.legacy,
            security_log: config.security_log,
            maximum_bytes: config.maximum_bytes,
            upload_timeout: Duration::from_millis(config.upload_timeout_ms),
            status_delay: Duration::from_millis(config.status_delay_ms),
            diagnostics_bytes: config.diagnostics_bytes,
            security_log_bytes: config.security_log_bytes,
            no_diagnostics: config.no_diagnostics,
            upload_failures: config.upload_failures,
            reject_get_log: config.reject_get_log,
            failure_status: config.get_log_failure_status,
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

const fn enabled() -> bool {
    true
}
