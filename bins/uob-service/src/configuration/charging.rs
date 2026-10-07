//! Opt-in demo-only station roster and native-to-canonical charging topology.
//! Secrets and persistent-state directory contents are resolved and protected by the runtime.
mod action_option;
mod diagnostics;
mod firmware;
pub(crate) use action_option::StationActionOption;
mod topology;
pub(crate) use diagnostics::StationDiagnostics;
pub(crate) use firmware::{StationFirmware, ValidatedFirmwareArtifacts};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use topology::validate_resources;

use serde::Deserialize;
use uob_application::{CredentialReference, DEFAULT_MAX_CONNECTED_STATIONS};
use uob_contracts::{BridgeId, Environment, ProtocolEdition, ResourceRef, StationId};

use super::ConfigurationLoadError;

const MAX_RESOURCES_PER_STATION: usize = 64;
const MAX_TOTAL_RESOURCES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

/// A missing section keeps all existing service configurations unchanged.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    enabled: bool,
    listen_addr: Option<SocketAddr>,
    state_directory: Option<String>,
    read_grant_file: Option<String>,
    control_grant_file: Option<String>,
    privileged_grant_file: Option<String>,
    #[serde(rename = "configuration_values_file")]
    values_file: Option<String>,
    local_authorization_updates_file: Option<String>,
    firmware: Option<firmware::FirmwareSection>,
    stations: Vec<StationConfiguration>,
}

/// Station actions are individually opt-in; their TOML keys remain on the station table.
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent operator-facing TOML key.
pub(crate) struct StationControlOptions {
    pub change_availability: bool,
    pub trigger_message: StationActionOption,
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
    /// Security Whitepaper `GetLog` (diagnostics or security log) on OCPP 1.6.
    pub get_log: StationActionOption,
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
    pub fn configuration_enabled(self) -> bool {
        self.set_variables.enabled() || self.set_network_profile.enabled()
    }
    pub fn local_authorization_enabled(self) -> bool {
        self.get_local_list_version.enabled()
            || self.send_local_list.enabled()
            || self.clear_cache.enabled()
    }
    fn requires_privileged_grant(self) -> bool {
        self.change_availability
            || self.trigger_message.enabled()
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
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StationConfiguration {
    id: String,
    protocol: ProtocolEdition,
    credential_file: String,
    resources: Vec<ResourceConfiguration>,
    start_token_file: Option<String>,
    reservation16_file: Option<String>,
    reservation201_file: Option<String>,
    firmware_job_timeout_seconds: Option<u64>,
    diagnostics_job_timeout_seconds: Option<u64>,
    diagnostics_upload_max_bytes: Option<u64>,
    #[serde(flatten)]
    control: StationControlOptions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceConfiguration {
    #[serde(rename = "evse_id")]
    evse: Option<String>,
    #[serde(rename = "connector_id")]
    connector: Option<String>,
    #[serde(rename = "native_evse_id")]
    native_evse: Option<u32>,
    #[serde(rename = "native_connector_id")]
    native_connector: Option<u32>,
}

/// A bounded, immutable roster. The read grant is scoped to exactly `stations`.
pub(crate) struct ValidatedChargingConfiguration {
    pub listen_addr: SocketAddr,
    /// Require a private, canonical, dedicated directory before opening its database.
    pub state_directory: PathBuf,
    /// Require a protected file at runtime; never embed bearer credentials in TOML.
    pub read_grant_file: CredentialReference,
    pub stations: Vec<ValidatedChargingStation>,
    pub control_grant_file: Option<CredentialReference>,
    pub privileged_grant_file: Option<CredentialReference>,
    pub configuration_values_file: Option<PathBuf>,
    pub local_authorization_updates_file: Option<PathBuf>,
    pub firmware: Option<ValidatedFirmwareArtifacts>,
}

pub(crate) struct ValidatedChargingStation {
    pub station_id: StationId,
    pub protocol: ProtocolEdition,
    /// Require a protected file at runtime; the protocol authenticator hashes its contents.
    pub credential_file: CredentialReference,
    /// Includes the station-only address, plus exactly the declared connector/EVSE addresses.
    pub resources: Vec<ResourceRef>,
    pub start_token_file: Option<CredentialReference>,
    pub control: StationControlOptions,
    pub reservation16_file: Option<PathBuf>,
    pub reservation201_file: Option<PathBuf>,
    pub firmware: Option<StationFirmware>,
    pub diagnostics: Option<StationDiagnostics>,
}

impl Configuration {
    pub(super) fn validate(
        self,
        environment: Environment,
        management_address: SocketAddr,
        bridge_name: &str,
    ) -> Result<Option<ValidatedChargingConfiguration>, ConfigurationLoadError> {
        let fail = ConfigurationLoadError::InvalidCharging;
        if !self.enabled {
            self.validate_disabled()?;
            return Ok(None);
        }
        if environment != Environment::Demo {
            return Err(fail);
        }
        let listen_addr = self.listen_addr.ok_or(fail)?;
        if !listen_addr.ip().is_loopback()
            || listen_addr == management_address
            || listen_addr.port() == 0
        {
            return Err(fail);
        }
        let state_directory = private_absolute_path(&self.state_directory.ok_or(fail)?)?;
        let grant_path = private_absolute_path(&self.read_grant_file.ok_or(fail)?)?;
        if grant_path == state_directory || grant_path.starts_with(&state_directory) {
            return Err(fail);
        }
        let read_grant_file = CredentialReference::new(grant_path.to_string_lossy().into_owned())
            .map_err(|_| fail)?;
        if self.privileged_grant_file.is_some() && self.control_grant_file.is_none() {
            return Err(fail);
        }
        if self.control_grant_file.is_none()
            && self.stations.iter().any(|station| {
                station.start_token_file.is_some()
                    || station.control.allow_stop
                    || station.control.allow_charging_limit
                    || station.control.requires_privileged_grant()
            })
        {
            return Err(fail);
        }
        if self.privileged_grant_file.is_none()
            && self
                .stations
                .iter()
                .any(|station| station.control.requires_privileged_grant())
        {
            return Err(fail);
        }
        let mut paths = BTreeSet::from([grant_path.clone()]);
        let mut credential =
            |value: Option<String>| -> Result<Option<CredentialReference>, ConfigurationLoadError> {
                value
                    .map(|value| {
                        let path = private_absolute_path(&value)?;
                        if path == state_directory
                            || path.starts_with(&state_directory)
                            || !paths.insert(path.clone())
                        {
                            return Err(fail);
                        }
                        CredentialReference::new(path.to_string_lossy().into_owned())
                            .map_err(|_| fail)
                    })
                    .transpose()
            };
        let control_grant_file = credential(self.control_grant_file)?;
        let privileged_grant_file = credential(self.privileged_grant_file)?;
        let configuration_values_file =
            credential(self.values_file)?.map(|file| PathBuf::from(file.as_str()));
        let local_authorization_updates_file = credential(self.local_authorization_updates_file)?
            .map(|file| PathBuf::from(file.as_str()));
        if self
            .stations
            .iter()
            .any(|station| station.control.send_local_list.enabled())
            && local_authorization_updates_file.is_none()
        {
            return Err(fail);
        }
        if self
            .stations
            .iter()
            .any(|station| station.control.configuration_enabled())
            && configuration_values_file.is_none()
        {
            return Err(fail);
        }
        let stations = validate_stations(self.stations, &state_directory, paths, bridge_name)?;
        let firmware = firmware::validate(
            self.firmware,
            &stations,
            &state_directory,
            &[listen_addr, management_address],
        )?;
        Ok(Some(ValidatedChargingConfiguration {
            listen_addr,
            state_directory,
            read_grant_file,
            stations,
            control_grant_file,
            privileged_grant_file,
            configuration_values_file,
            local_authorization_updates_file,
            firmware,
        }))
    }

    fn validate_disabled(&self) -> Result<(), ConfigurationLoadError> {
        if self.listen_addr.is_none()
            && self.state_directory.is_none()
            && self.read_grant_file.is_none()
            && self.control_grant_file.is_none()
            && self.privileged_grant_file.is_none()
            && self.values_file.is_none()
            && self.local_authorization_updates_file.is_none()
            && self.firmware.is_none()
            && self.stations.is_empty()
        {
            Ok(())
        } else {
            Err(ConfigurationLoadError::InvalidCharging)
        }
    }
}

#[allow(clippy::too_many_lines)] // Cross-field station checks stay together so none is skipped.
fn validate_stations(
    entries: Vec<StationConfiguration>,
    state_directory: &Path,
    grant_paths: BTreeSet<PathBuf>,
    bridge_name: &str,
) -> Result<Vec<ValidatedChargingStation>, ConfigurationLoadError> {
    let fail = ConfigurationLoadError::InvalidCharging;
    if entries.is_empty() || entries.len() > DEFAULT_MAX_CONNECTED_STATIONS {
        return Err(fail);
    }
    let bridge_id = BridgeId::new(bridge_name.to_owned()).map_err(|_| fail)?;
    let mut station_ids = BTreeSet::new();
    let mut credential_files = grant_paths;
    let mut stations = Vec::with_capacity(entries.len());
    let mut total_resources = 0;
    for station in entries {
        if (station.control.device_model_enabled() || station.control.configuration_enabled())
            && station.protocol != ProtocolEdition::Ocpp201
        {
            return Err(fail);
        }
        // Each edition owns its reservation provider and capacity option; neither is shared.
        if (station.control.reserve_connector_zero_supported
            || station.reservation16_file.is_some())
            && station.protocol != ProtocolEdition::Ocpp16j
        {
            return Err(fail);
        }
        if (station.control.reserve_non_evse_specific_supported
            || station.reservation201_file.is_some())
            && station.protocol != ProtocolEdition::Ocpp201
        {
            return Err(fail);
        }
        let reservation_file = match station.protocol {
            ProtocolEdition::Ocpp16j => station.reservation16_file.is_some(),
            ProtocolEdition::Ocpp201 => station.reservation201_file.is_some(),
        };
        if station.control.reserve_now.enabled() && !reservation_file {
            return Err(fail);
        }
        if station.control.get_charging_profiles.enabled()
            && station.protocol != ProtocolEdition::Ocpp201
        {
            return Err(fail);
        }
        // Processing obliges a later TxProfile: require a path that can send one.
        if station.control.ev_charging_needs_processing
            && (station.protocol != ProtocolEdition::Ocpp201
                || !(station.control.allow_charging_limit
                    || station.control.set_charging_profile.enabled()))
        {
            return Err(fail);
        }
        if (station.control.get_composite_schedule.enabled()
            || station.control.get_charging_profiles.enabled()
            || station.control.charging_profiles_enabled()
            || station.control.configuration_enabled())
            && station.resources.iter().any(|resource| {
                resource
                    .native_evse
                    .is_some_and(|id| i32::try_from(id).is_err())
                    || resource
                        .native_connector
                        .is_some_and(|id| i32::try_from(id).is_err())
            })
        {
            return Err(fail);
        }
        let station_id = StationId::new(valid_station_name(station.id)?).map_err(|_| fail)?;
        if !station_ids.insert(station_id.clone()) {
            return Err(fail);
        }
        let path = private_absolute_path(&station.credential_file)?;
        if path == state_directory
            || path.starts_with(state_directory)
            || !credential_files.insert(path.clone())
        {
            return Err(fail);
        }
        let credential_file =
            CredentialReference::new(path.to_string_lossy().into_owned()).map_err(|_| fail)?;
        let start_token_file = station
            .start_token_file
            .map(|value| {
                let path = private_absolute_path(&value)?;
                if path == state_directory
                    || path.starts_with(state_directory)
                    || !credential_files.insert(path.clone())
                {
                    return Err(fail);
                }
                CredentialReference::new(path.to_string_lossy().into_owned()).map_err(|_| fail)
            })
            .transpose()?;
        let mut provider_file = |value: Option<String>| {
            value
                .map(|value| {
                    let path = private_absolute_path(&value)?;
                    if path == state_directory
                        || path.starts_with(state_directory)
                        || !credential_files.insert(path.clone())
                    {
                        return Err(fail);
                    }
                    Ok(path)
                })
                .transpose()
        };
        let firmware = firmware::station(
            &station.control,
            station.firmware_job_timeout_seconds,
            station.protocol,
        )?;
        let diagnostics = diagnostics::station(
            &station.control,
            station.diagnostics_job_timeout_seconds,
            station.diagnostics_upload_max_bytes,
            station.protocol,
        )?;
        let reservation16_file = provider_file(station.reservation16_file)?;
        let reservation201_file = provider_file(station.reservation201_file)?;
        if station.resources.is_empty() || station.resources.len() > MAX_RESOURCES_PER_STATION {
            return Err(fail);
        }
        total_resources += station.resources.len();
        if total_resources > MAX_TOTAL_RESOURCES {
            return Err(fail);
        }
        let resources =
            validate_resources(station.resources, station.protocol, &bridge_id, &station_id)?;
        stations.push(ValidatedChargingStation {
            station_id,
            protocol: station.protocol,
            credential_file,
            start_token_file,
            control: station.control,
            resources,
            reservation16_file,
            reservation201_file,
            firmware,
            diagnostics,
        });
    }
    Ok(stations)
}

/// Checks syntax only. The runtime must verify ownership, privacy, symlinks and existence
/// when opening credential files and the persistent directory (including on restart).
fn private_absolute_path(value: &str) -> Result<PathBuf, ConfigurationLoadError> {
    let path = Path::new(value);
    if value.len() > MAX_PATH_BYTES
        || value.chars().any(char::is_control)
        || value.trim() != value
        || !path.is_absolute()
        || path.file_name().is_none()
        || value.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(ConfigurationLoadError::InvalidCharging);
    }
    Ok(path.to_path_buf())
}

fn valid_station_name(value: String) -> Result<String, ConfigurationLoadError> {
    if value.len() > 64
        || value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return Err(ConfigurationLoadError::InvalidCharging);
    }
    Ok(value)
}

fn valid_resource_name(value: String) -> Result<String, ConfigurationLoadError> {
    if value.len() > 128 || value.trim() != value || value.chars().any(char::is_control) {
        return Err(ConfigurationLoadError::InvalidCharging);
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
