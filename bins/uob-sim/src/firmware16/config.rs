//! Operator-facing station firmware options and the fault knobs copied into the live model.
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAXIMUM_BYTES_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareMode {
    Legacy,
    Signed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CancelPolicy {
    #[default]
    Cancel,
    Reject,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent operator-facing TOML key.
pub struct FirmwareConfig {
    pub private_state_file: String,
    pub mode: FirmwareMode,
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
    #[serde(default)]
    pub reject_signed: bool,
    #[serde(default)]
    pub cancel_policy: CancelPolicy,
    #[serde(default)]
    pub status_delay_ms: u64,
    #[serde(default = "enabled")]
    pub wait_for_transactions: bool,
}

impl FirmwareConfig {
    /// # Errors
    /// Rejects bounds outside the simulator's supported ranges and mode-specific mismatches.
    pub fn validate(&self) -> Result<(), &'static str> {
        let signed = self.mode == FirmwareMode::Signed;
        if self.maximum_bytes == 0
            || self.maximum_bytes > MAXIMUM_BYTES_LIMIT
            || self.download_timeout_ms == 0
            || self.download_timeout_ms > 300_000
            || self.status_delay_ms > 30_000
            || self.download_failures > 16
            || self.firmware_version_prefix.len() > 20
            || !self
                .firmware_version_prefix
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
            || signed != self.manufacturer_root_file.is_some()
            || (!signed && (self.fail_install_verification || self.reject_signed))
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
    pub(crate) mode: FirmwareMode,
    pub(crate) maximum_bytes: u64,
    pub(crate) download_timeout: Duration,
    pub(crate) reboot_after_install: bool,
    pub(crate) firmware_version_prefix: String,
    pub(crate) download_failures: u32,
    pub(crate) fail_install: bool,
    pub(crate) fail_install_verification: bool,
    pub(crate) reject_signed: bool,
    pub(crate) cancel_policy: CancelPolicy,
    pub(crate) status_delay: Duration,
    pub(crate) wait_for_transactions: bool,
}

impl From<&FirmwareConfig> for Settings {
    fn from(config: &FirmwareConfig) -> Self {
        Self {
            mode: config.mode,
            maximum_bytes: config.maximum_bytes,
            download_timeout: Duration::from_millis(config.download_timeout_ms),
            reboot_after_install: config.reboot_after_install,
            firmware_version_prefix: config.firmware_version_prefix.clone(),
            download_failures: config.download_failures,
            fail_install: config.fail_install,
            fail_install_verification: config.fail_install_verification,
            reject_signed: config.reject_signed,
            cancel_policy: config.cancel_policy,
            status_delay: Duration::from_millis(config.status_delay_ms),
            wait_for_transactions: config.wait_for_transactions,
        }
    }
}

/// Public manufacturer roots from an absolute, bounded PEM file.
pub(crate) fn read_roots(path: &str) -> Result<super::verify::TrustRoots, &'static str> {
    use std::io::Read;
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err("firmware_root_path");
    }
    let file = std::fs::File::open(path).map_err(|_| "firmware_root_unreadable")?;
    let metadata = file.metadata().map_err(|_| "firmware_root_unreadable")?;
    if !metadata.is_file() || metadata.len() > super::verify::ROOT_FILE_LIMIT {
        return Err("firmware_root_invalid");
    }
    let mut bytes = Vec::new();
    file.take(super::verify::ROOT_FILE_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|_| "firmware_root_unreadable")?;
    super::verify::TrustRoots::parse(&bytes)
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
