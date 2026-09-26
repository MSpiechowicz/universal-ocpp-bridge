//! Opt-in demo-only station roster and native-to-canonical charging topology.
//! Secrets and persistent-state directory contents are resolved and protected by the runtime.
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use uob_application::{CredentialReference, DEFAULT_MAX_CONNECTED_STATIONS};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource, Environment,
    NativeProtocolReference, ProtocolEdition, ResourceRef, StationId,
};

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
    stations: Vec<StationConfiguration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StationConfiguration {
    id: String,
    protocol: ProtocolEdition,
    credential_file: String,
    resources: Vec<ResourceConfiguration>,
    #[serde(default)]
    change_availability: bool,
    start_token_file: Option<String>,
    #[serde(default)]
    allow_stop: bool,
    #[serde(default)]
    allow_charging_limit: bool,
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
}

pub(crate) struct ValidatedChargingStation {
    pub station_id: StationId,
    pub protocol: ProtocolEdition,
    /// Require a protected file at runtime; the protocol authenticator hashes its contents.
    pub credential_file: CredentialReference,
    /// Includes the station-only address, plus exactly the declared connector/EVSE addresses.
    pub resources: Vec<ResourceRef>,
    pub change_availability: bool,
    pub start_token_file: Option<CredentialReference>,
    pub allow_stop: bool,
    pub allow_charging_limit: bool,
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
            return if self.listen_addr.is_none()
                && self.state_directory.is_none()
                && self.read_grant_file.is_none()
                && self.control_grant_file.is_none()
                && self.privileged_grant_file.is_none()
                && self.stations.is_empty()
            {
                Ok(None)
            } else {
                Err(fail)
            };
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
                    || station.allow_stop
                    || station.allow_charging_limit
                    || station.change_availability
            })
        {
            return Err(fail);
        }
        if self.privileged_grant_file.is_none()
            && self
                .stations
                .iter()
                .any(|station| station.change_availability)
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
        let stations = validate_stations(self.stations, &state_directory, paths, bridge_name)?;
        Ok(Some(ValidatedChargingConfiguration {
            listen_addr,
            state_directory,
            read_grant_file,
            stations,
            control_grant_file,
            privileged_grant_file,
        }))
    }
}

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
            change_availability: station.change_availability,
            start_token_file,
            allow_stop: station.allow_stop,
            allow_charging_limit: station.allow_charging_limit,
            resources,
        });
    }
    Ok(stations)
}

fn validate_resources(
    entries: Vec<ResourceConfiguration>,
    protocol: ProtocolEdition,
    bridge_id: &BridgeId,
    station_id: &StationId,
) -> Result<Vec<ResourceRef>, ConfigurationLoadError> {
    let fail = ConfigurationLoadError::InvalidCharging;
    let mut native_addresses = BTreeSet::new();
    let mut native_evse_map = BTreeMap::new();
    let mut canonical_evse_map = BTreeMap::new();
    let mut canonical_addresses = BTreeSet::new();
    let mut resources = Vec::with_capacity(entries.len() + 1);
    resources.push(ResourceRef {
        bridge_id: bridge_id.clone(),
        station_id: station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    });
    for resource in entries {
        let (canonical, native, canonical_key, native_key) = match protocol {
            ProtocolEdition::Ocpp16j => {
                if resource.evse.is_some() || resource.native_evse.is_some() {
                    return Err(fail);
                }
                let connector = CanonicalConnectorId::new(valid_resource_name(
                    resource.connector.ok_or(fail)?,
                )?)
                .map_err(|_| fail)?;
                let number = resource.native_connector.filter(|id| *id > 0).ok_or(fail)?;
                let canonical_key = (None, Some(connector.as_str().to_owned()));
                (
                    CanonicalResource::Connector {
                        connector_id: connector,
                    },
                    NativeProtocolReference::Ocpp16 {
                        connector_id: number,
                    },
                    canonical_key,
                    (number, None),
                )
            }
            ProtocolEdition::Ocpp201 => {
                let evse = CanonicalEvseId::new(valid_resource_name(resource.evse.ok_or(fail)?)?)
                    .map_err(|_| fail)?;
                let native_evse = resource.native_evse.filter(|id| *id > 0).ok_or(fail)?;
                if native_evse_map
                    .insert(native_evse, evse.as_str().to_owned())
                    .is_some_and(|previous| previous != evse.as_str())
                    || canonical_evse_map
                        .insert(evse.as_str().to_owned(), native_evse)
                        .is_some_and(|previous| previous != native_evse)
                {
                    return Err(fail);
                }
                let connector = resource
                    .connector
                    .map(valid_resource_name)
                    .transpose()?
                    .map(CanonicalConnectorId::new)
                    .transpose()
                    .map_err(|_| fail)?;
                let native_connector = resource.native_connector;
                if connector.is_some() != native_connector.is_some() || native_connector == Some(0)
                {
                    return Err(fail);
                }
                let canonical_key = (
                    Some(evse.as_str().to_owned()),
                    connector.as_ref().map(|c| c.as_str().to_owned()),
                );
                (
                    CanonicalResource::Evse {
                        evse_id: evse,
                        connector_id: connector,
                    },
                    NativeProtocolReference::Ocpp201 {
                        evse_id: native_evse,
                        connector_id: native_connector,
                    },
                    canonical_key,
                    (native_evse, native_connector),
                )
            }
        };
        if !native_addresses.insert(native_key) || !canonical_addresses.insert(canonical_key) {
            return Err(fail);
        }
        resources.push(ResourceRef {
            bridge_id: bridge_id.clone(),
            station_id: station_id.clone(),
            resource: Some(canonical),
            native_protocol_reference: Some(native),
        });
    }
    Ok(resources)
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
