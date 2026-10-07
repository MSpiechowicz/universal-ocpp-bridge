//! Operator-facing OCPP 2.0.1 station firmware options and the fault knobs copied into the model.
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAXIMUM_BYTES_LIMIT: u64 = 256 * 1024 * 1024;

/// L01 Secure Firmware Update or L02 Non-Secure Firmware Update; one per station.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareMode201 {
    Secure,
    NonSecure,
}

/// L01.FR.24 cancels an ongoing update; L01.FR.27 lets a station unable to cancel reject.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CancelPolicy201 {
    #[default]
    Cancel,
    Reject,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent operator-facing TOML key.
pub struct FirmwareConfig201 {
    pub private_state_file: String,
    pub mode: FirmwareMode201,
    /// Installed manufacturer roots; required exactly for a secure station.
    pub manufacturer_root_file: Option<String>,
    #[serde(default = "default_maximum_bytes")]
    pub maximum_bytes: u64,
    #[serde(default = "default_download_timeout_ms")]
    pub download_timeout_ms: u64,
    #[serde(default = "enabled")]
    pub reboot_after_install: bool,
    #[serde(default)]
    pub firmware_version_prefix: String,
    #[serde(default)]
    pub download_failures: u32,
    #[serde(default)]
    pub fail_install: bool,
    #[serde(default)]
    pub fail_install_verification: bool,
    /// Answer every new request `Rejected`.
    #[serde(default)]
    pub reject_updates: bool,
    #[serde(default)]
    pub cancel_policy: CancelPolicy201,
    #[serde(default)]
    pub status_delay_ms: u64,
    #[serde(default = "enabled")]
    pub wait_for_transactions: bool,
}

impl FirmwareConfig201 {
    /// # Errors
    /// Rejects bounds outside the simulator's supported ranges and mode-specific mismatches.
    pub fn validate(&self) -> Result<(), &'static str> {
        let secure = self.mode == FirmwareMode201::Secure;
        if self.maximum_bytes == 0
            || self.maximum_bytes > MAXIMUM_BYTES_LIMIT
            || self.download_timeout_ms == 0
            || self.download_timeout_ms > 300_000
            || self.status_delay_ms > 30_000
            || self.download_failures > 16
            // BootNotification firmwareVersion is CiString50; the digest suffix uses 19.
            || self.firmware_version_prefix.len() > 31
            || !self
                .firmware_version_prefix
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
            || secure != self.manufacturer_root_file.is_some()
        {
            return Err("firmware_config_invalid");
        }
        Ok(())
    }
}

/// Fault and timing knobs copied from configuration.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // Mirrors the independent configuration flags.
pub(crate) struct Settings {
    pub(crate) mode: FirmwareMode201,
    pub(crate) maximum_bytes: u64,
    pub(crate) download_timeout: Duration,
    pub(crate) reboot_after_install: bool,
    pub(crate) firmware_version_prefix: String,
    pub(crate) download_failures: u32,
    pub(crate) fail_install: bool,
    pub(crate) fail_install_verification: bool,
    pub(crate) reject_updates: bool,
    pub(crate) cancel_policy: CancelPolicy201,
    pub(crate) status_delay: Duration,
    pub(crate) wait_for_transactions: bool,
}

impl From<&FirmwareConfig201> for Settings {
    fn from(config: &FirmwareConfig201) -> Self {
        Self {
            mode: config.mode,
            maximum_bytes: config.maximum_bytes,
            download_timeout: Duration::from_millis(config.download_timeout_ms),
            reboot_after_install: config.reboot_after_install,
            firmware_version_prefix: config.firmware_version_prefix.clone(),
            download_failures: config.download_failures,
            fail_install: config.fail_install,
            fail_install_verification: config.fail_install_verification,
            reject_updates: config.reject_updates,
            cancel_policy: config.cancel_policy,
            status_delay: Duration::from_millis(config.status_delay_ms),
            wait_for_transactions: config.wait_for_transactions,
        }
    }
}

const fn default_maximum_bytes() -> u64 {
    64 * 1024 * 1024
}

const fn default_download_timeout_ms() -> u64 {
    30_000
}

const fn enabled() -> bool {
    true
}
