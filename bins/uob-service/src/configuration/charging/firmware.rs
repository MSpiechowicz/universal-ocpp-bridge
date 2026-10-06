//! Demo-only OCPP 1.6 firmware options: one native family per station and one local test
//! artifact service. Production never reaches this section because charging is demo-only.
use std::{net::SocketAddr, path::PathBuf, time::Duration};

use serde::Deserialize;
use uob_contracts::ProtocolEdition;

use super::{
    ConfigurationLoadError, StationControlOptions, ValidatedChargingStation, private_absolute_path,
};

const MIN_JOB_TIMEOUT_SECONDS: u64 = 60;
const MAX_JOB_TIMEOUT_SECONDS: u64 = 7 * 24 * 60 * 60;

/// `[charging.firmware]`: where stations download test firmware and what is published.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FirmwareSection {
    listen_addr: SocketAddr,
    /// Station-reachable base; defaults to `http://<listen_addr>`.
    public_base: Option<String>,
    spool_directory: String,
    catalog_file: String,
    /// Optional public output of the generated demo manufacturer root for station provisioning.
    manufacturer_root_file: Option<String>,
    organization: Option<String>,
}

pub(crate) struct ValidatedFirmwareArtifacts {
    pub listen_addr: SocketAddr,
    pub public_base: String,
    pub spool_directory: PathBuf,
    pub catalog_file: PathBuf,
    pub manufacturer_root_file: Option<PathBuf>,
    pub organization: String,
}

/// Exactly one native family; a signed station answers `UpdateFirmware` with `NotSupported`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StationFirmware {
    pub signed: bool,
    pub job_timeout: Duration,
}

pub(super) fn station(
    control: &StationControlOptions,
    timeout_seconds: Option<u64>,
    protocol: ProtocolEdition,
) -> Result<Option<StationFirmware>, ConfigurationLoadError> {
    let fail = ConfigurationLoadError::InvalidCharging;
    let (legacy, signed) = (
        control.update_firmware.enabled(),
        control.signed_update_firmware.enabled(),
    );
    if !legacy && !signed {
        return if timeout_seconds.is_some() {
            Err(fail)
        } else {
            Ok(None)
        };
    }
    let timeout = timeout_seconds.ok_or(fail)?;
    if (legacy && signed)
        || protocol != ProtocolEdition::Ocpp16j
        || !(MIN_JOB_TIMEOUT_SECONDS..=MAX_JOB_TIMEOUT_SECONDS).contains(&timeout)
    {
        return Err(fail);
    }
    Ok(Some(StationFirmware {
        signed,
        job_timeout: Duration::from_secs(timeout),
    }))
}

pub(super) fn validate(
    section: Option<FirmwareSection>,
    stations: &[ValidatedChargingStation],
    state_directory: &std::path::Path,
    occupied: &[SocketAddr],
) -> Result<Option<ValidatedFirmwareArtifacts>, ConfigurationLoadError> {
    let fail = ConfigurationLoadError::InvalidCharging;
    let used = stations.iter().any(|station| station.firmware.is_some());
    let Some(section) = section else {
        return if used { Err(fail) } else { Ok(None) };
    };
    // No listener without a station that can use it.
    if !used
        || !section.listen_addr.ip().is_loopback()
        || section.listen_addr.port() == 0
        || occupied.contains(&section.listen_addr)
    {
        return Err(fail);
    }
    let private = |value: &str| {
        let path = private_absolute_path(value)?;
        if path == state_directory || path.starts_with(state_directory) {
            return Err(fail);
        }
        Ok(path)
    };
    let spool_directory = private(&section.spool_directory)?;
    let catalog_file = private(&section.catalog_file)?;
    let manufacturer_root_file = section
        .manufacturer_root_file
        .as_deref()
        .map(private)
        .transpose()?;
    if catalog_file.starts_with(&spool_directory)
        || manufacturer_root_file
            .as_ref()
            .is_some_and(|path| path == &catalog_file || path.starts_with(&spool_directory))
    {
        return Err(fail);
    }
    let public_base = section
        .public_base
        .unwrap_or_else(|| format!("http://{}", section.listen_addr));
    let organization = section
        .organization
        .unwrap_or_else(|| "Universal OCPP Bridge Demo".to_owned());
    Ok(Some(ValidatedFirmwareArtifacts {
        listen_addr: section.listen_addr,
        public_base,
        spool_directory,
        catalog_file,
        manufacturer_root_file,
        organization,
    }))
}

#[cfg(test)]
#[path = "firmware_tests.rs"]
mod tests;
