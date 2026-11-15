//! Operator-facing security-event options, fault knobs and per-message `ExtendedTriggerMessage`
//! replies, copied into the live model.
use crate::TriggerReply;
use serde::Deserialize;
use std::time::Duration;

const REPLY_DELAY_LIMIT_MS: u64 = 30_000;

/// How the station answers each `ExtendedTriggerMessage.requestedMessage` (Whitepaper §5.5/§5.6).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtendedTriggerConfig {
    pub boot_notification: TriggerReply,
    pub log_status_notification: TriggerReply,
    pub firmware_status_notification: TriggerReply,
    pub heartbeat: TriggerReply,
    pub meter_values: TriggerReply,
    /// Certificate signing is not modelled: the default answer is `NotImplemented`.
    pub sign_charge_point_certificate: TriggerReply,
    pub status_notification: TriggerReply,
    /// Holds the `ExtendedTriggerMessage.conf` for this long before it is written.
    pub reply_delay_ms: u64,
    /// Answer `Accepted` but never send the triggered message.
    pub omit: bool,
}

impl Default for ExtendedTriggerConfig {
    fn default() -> Self {
        Self {
            boot_notification: TriggerReply::Accepted,
            log_status_notification: TriggerReply::Accepted,
            firmware_status_notification: TriggerReply::Accepted,
            heartbeat: TriggerReply::Accepted,
            meter_values: TriggerReply::Accepted,
            sign_charge_point_certificate: TriggerReply::NotImplemented,
            status_notification: TriggerReply::Accepted,
            reply_delay_ms: 0,
            omit: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Owner-only file holding the durable security-event outbox.
    pub private_state_file: String,
    /// Fault knob: after every acknowledged event, send an identical copy once more so the
    /// CSMS has to deduplicate.
    #[serde(default)]
    pub duplicate_acknowledged: bool,
    /// Fault knob: deliver queued events even before the CSMS accepted this connection's Boot.
    #[serde(default)]
    pub send_before_accepted: bool,
    #[serde(default)]
    pub extended_trigger: ExtendedTriggerConfig,
}

impl SecurityConfig {
    /// # Errors
    /// Rejects an empty state path and a reply delay outside the supported range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.private_state_file.is_empty()
            || self.extended_trigger.reply_delay_ms > REPLY_DELAY_LIMIT_MS
        {
            return Err("security_config_invalid");
        }
        Ok(())
    }
}

/// Fault and timing knobs copied from configuration.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) duplicate_acknowledged: bool,
    pub(crate) send_before_accepted: bool,
    pub(crate) trigger: ExtendedTriggerConfig,
    pub(crate) reply_delay: Duration,
}

impl From<&SecurityConfig> for Settings {
    fn from(config: &SecurityConfig) -> Self {
        Self {
            duplicate_acknowledged: config.duplicate_acknowledged,
            send_before_accepted: config.send_before_accepted,
            trigger: config.extended_trigger.clone(),
            reply_delay: Duration::from_millis(config.extended_trigger.reply_delay_ms),
        }
    }
}
