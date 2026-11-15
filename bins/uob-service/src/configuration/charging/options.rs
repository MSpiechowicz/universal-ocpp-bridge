//! Operator-facing station action options; each TOML key lives on the station table.
use super::StationActionOption;
use serde::Deserialize;

/// Station actions are individually opt-in; their TOML keys remain on the station table.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent operator-facing TOML key.
pub(crate) struct StationControlOptions {
    pub change_availability: bool,
    pub trigger_message: StationActionOption,
    /// OCPP 1.6 Security Whitepaper `ExtendedTriggerMessage`; refused for OCPP 2.0.1. Charger
    /// initiated `SecurityEventNotification` is always answered and needs no option.
    pub extended_trigger_message: StationActionOption,
    pub get_composite_schedule: StationActionOption,
    pub get_charging_profiles: StationActionOption,
    pub set_charging_profile: StationActionOption,
    pub clear_charging_profile: StationActionOption,
    pub get_variables: StationActionOption,
    pub get_base_report: StationActionOption,
    pub get_report: StationActionOption,
    pub set_variables: StationActionOption,
    pub set_network_profile: StationActionOption,
    pub get_local_list_version: StationActionOption,
    pub send_local_list: StationActionOption,
    pub clear_cache: StationActionOption,
    pub reserve_now: StationActionOption,
    pub cancel_reservation: StationActionOption,
    /// `UpdateFirmware`. On OCPP 1.6 it is exclusive with the Security Whitepaper variant; on
    /// OCPP 2.0.1 it is a secure update (L01) unless `non_secure_firmware` selects L02.
    pub update_firmware: StationActionOption,
    pub signed_update_firmware: StationActionOption,
    /// OCPP 1.6 `GetDiagnostics`; uploads go to the `[charging.firmware]` artifact service.
    pub get_diagnostics: StationActionOption,
    /// `GetLog` (diagnostics or security log): the Security Whitepaper message on OCPP 1.6 and
    /// the native N01 message on OCPP 2.0.1.
    pub get_log: StationActionOption,
    /// OCPP 2.0.1 variable monitoring (N02 to N06), station-scoped. Charger-initiated
    /// `NotifyEvent` is always answered and needs no option.
    pub set_variable_monitoring: StationActionOption,
    pub clear_variable_monitoring: StationActionOption,
    pub get_monitoring_report: StationActionOption,
    pub set_monitoring_base: StationActionOption,
    pub set_monitoring_level: StationActionOption,
    /// OCPP 2.0.1 only: send firmware without signing certificate or signature (L02).
    pub non_secure_firmware: bool,
    pub reserve_connector_zero_supported: bool,
    /// OCPP 2.0.1 `ReservationCtrlr.NonEvseSpecific`; never inferred from the station.
    pub reserve_non_evse_specific_supported: bool,
    pub allow_stop: bool,
    pub allow_charging_limit: bool,
    /// OCPP 2.0.1 only: answer charging needs with `Processing` because the operator's EMS
    /// sends a `TxProfile` through this bridge (K15.FR.05/07/08). Default `Rejected`.
    pub ev_charging_needs_processing: bool,
}

impl StationControlOptions {
    pub fn charging_profiles_enabled(self) -> bool {
        self.set_charging_profile.enabled() || self.clear_charging_profile.enabled()
    }
    pub fn device_model_enabled(self) -> bool {
        self.get_variables.enabled() || self.get_base_report.enabled() || self.get_report.enabled()
    }
    pub fn monitoring_enabled(self) -> bool {
        self.set_variable_monitoring.enabled()
            || self.clear_variable_monitoring.enabled()
            || self.get_monitoring_report.enabled()
            || self.set_monitoring_base.enabled()
            || self.set_monitoring_level.enabled()
    }
    /// Whether the station may ask the charger to send one of its own messages.
    pub fn trigger_enabled(self) -> bool {
        self.trigger_message.enabled() || self.extended_trigger_message.enabled()
    }
    pub fn configuration_enabled(self) -> bool {
        self.set_variables.enabled() || self.set_network_profile.enabled()
    }
    pub fn local_authorization_enabled(self) -> bool {
        self.get_local_list_version.enabled()
            || self.send_local_list.enabled()
            || self.clear_cache.enabled()
    }
    pub(super) fn requires_privileged_grant(self) -> bool {
        self.change_availability
            || self.trigger_message.enabled()
            || self.extended_trigger_message.enabled()
            || self.get_composite_schedule.enabled()
            || self.get_charging_profiles.enabled()
            || self.charging_profiles_enabled()
            || self.device_model_enabled()
            || self.configuration_enabled()
            || self.local_authorization_enabled()
            || self.reserve_now.enabled()
            || self.cancel_reservation.enabled()
            || self.update_firmware.enabled()
            || self.signed_update_firmware.enabled()
            || self.get_diagnostics.enabled()
            || self.get_log.enabled()
            || self.monitoring_enabled()
    }
}
