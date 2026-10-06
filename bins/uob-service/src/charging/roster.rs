//! Startup roster credentials and exact native/canonical station addresses.
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
};
use uob_contracts::{ProtocolEdition, ResourceRef, StationId};
use uob_protocol_adapter::{StationCredential, StationRegistration};

use super::{StationSettings, files};
use crate::configuration::charging::ValidatedChargingStation;

pub(super) struct StationRoster {
    pub(super) registrations: Vec<(StationRegistration, ProtocolEdition, StationCredential)>,
    pub(super) resources: BTreeMap<StationId, Vec<ResourceRef>>,
    pub(super) roster: Vec<ResourceRef>,
    pub(super) settings: BTreeMap<StationId, StationSettings>,
    pub(super) tokens: Vec<(StationId, ProtocolEdition, ResourceRef, files::ReadGrant)>,
}

pub(super) fn load_stations(
    stations: Vec<ValidatedChargingStation>,
    seen: &mut BTreeSet<(u64, u64)>,
) -> io::Result<StationRoster> {
    let mut registrations = Vec::with_capacity(stations.len());
    let mut resources = BTreeMap::new();
    let mut roster = Vec::new();
    let mut settings = BTreeMap::new();
    let mut tokens = Vec::new();
    for station in stations {
        let path = PathBuf::from(station.credential_file.as_str());
        let mut secret = files::secret(&path, seen).map_err(io::Error::other)?;
        let credential = StationCredential::from_secret(&secret);
        secret.fill(0);
        let credential = credential.map_err(io::Error::other)?;
        registrations.push((
            StationRegistration {
                station_id: station.station_id.clone(),
                credential: station.credential_file,
                client_certificate: None,
            },
            station.protocol,
            credential,
        ));
        roster.push(station.resources[0].clone());
        if let Some(file) = &station.start_token_file {
            let bytes =
                files::secret(&PathBuf::from(file.as_str()), seen).map_err(io::Error::other)?;
            tokens.push((
                station.station_id.clone(),
                station.protocol,
                station.resources[0].clone(),
                files::grant(bytes),
            ));
        }
        let reservations = station
            .reservation16_file
            .as_ref()
            .map(|file| {
                let mut bytes = files::protected(file, 65536, seen).map_err(io::Error::other)?;
                uob_protocol_adapter::v16::remote_control::ReservationValues16::from_json_bytes(
                    station.resources[0].clone(),
                    std::mem::take(&mut bytes.0),
                )
                .map(std::sync::Arc::new)
                .map_err(|error| io::Error::other(error.context().to_owned()))
            })
            .transpose()?;
        let reservations_201 = station
            .reservation201_file
            .as_ref()
            .map(|file| {
                let mut bytes = files::protected(file, 65536, seen).map_err(io::Error::other)?;
                uob_protocol_adapter::v201::remote_control::ReservationValues201::from_json_bytes(
                    station.resources[0].clone(),
                    std::mem::take(&mut bytes.0),
                )
                .map(std::sync::Arc::new)
                .map_err(|error| io::Error::other(error.context().to_owned()))
            })
            .transpose()?;
        settings.insert(
            station.station_id.clone(),
            StationSettings {
                protocol: station.protocol,
                start: None,
                control: station.control,
                configuration: None,
                local_authorization: None,
                local_authorization_201: None,
                reservations,
                reservations_201,
            },
        );
        resources.insert(station.station_id, station.resources);
    }
    Ok(StationRoster {
        registrations,
        resources,
        roster,
        settings,
        tokens,
    })
}
