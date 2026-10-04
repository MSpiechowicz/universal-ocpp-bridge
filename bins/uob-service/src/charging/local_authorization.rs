//! Owner-only startup updates, independent of the service authorization allowlist.
use super::{StationSettings, configuration201::SecretJson, files};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    sync::Arc,
};
use uob_contracts::{ProtocolEdition, ResourceRef, StationId, UtcTimestamp};
use uob_protocol_adapter::{v16::remote_control as v16, v201::remote_control as v201};

#[derive(Clone, Default)]
pub(super) struct Providers {
    pub v16: Option<Arc<v16::LocalAuthorizationUpdates16>>,
    pub v201: Option<Arc<v201::LocalAuthorizationUpdates201>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provisioning {
    updates: Vec<Update>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    station_id: StationId,
    #[serde(rename = "update_reference")]
    reference: String,
    expires_at: UtcTimestamp,
    request: SecretJson,
}
pub(super) fn install(
    path: Option<&Path>,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
    settings: &mut BTreeMap<StationId, StationSettings>,
    seen: &mut BTreeSet<(u64, u64)>,
) -> io::Result<Providers> {
    let Some(path) = path else {
        return Ok(Providers::default());
    };
    let fail = || io::Error::other("charging protected local authorization unavailable");
    let bytes = files::protected(path, 2 * 1024 * 1024, seen).map_err(|_| fail())?;
    let provisioning: Provisioning = serde_json::from_slice(&bytes.0).map_err(|_| fail())?;
    if provisioning.updates.len() > 128 {
        return Err(fail());
    }
    let mut values16 = Vec::new();
    let mut values201 = Vec::new();
    let mut stations16 = BTreeSet::new();
    let mut stations201 = BTreeSet::new();
    let mut retained = 8192usize;
    for mut entry in provisioning.updates {
        let station = settings.get(&entry.station_id).ok_or_else(fail)?;
        if !station.control.send_local_list.enabled() {
            return Err(fail());
        }
        let resource = resources
            .get(&entry.station_id)
            .and_then(|resources| resources.first())
            .ok_or_else(fail)?;
        let native = entry.request.take_bytes();
        retained = retained
            .checked_add(
                native.len()
                    + 2 * entry.reference.capacity()
                    + resource.bridge_id.as_str().len()
                    + resource.station_id.as_str().len()
                    + 1024,
            )
            .ok_or_else(fail)?;
        if retained > 1024 * 1024 {
            let mut native = native;
            native.fill(0);
            std::hint::black_box(native);
            return Err(fail());
        }
        match station.protocol {
            ProtocolEdition::Ocpp16j => {
                let update =
                    v16::ProtectedLocalListUpdate16::from_json_bytes(native).map_err(|_| fail())?;
                values16.push(v16::ProtectedLocalListValue16 {
                    resource: resource.clone(),
                    reference: entry.reference,
                    expires_at: entry.expires_at,
                    update,
                });
                stations16.insert(entry.station_id);
            }
            ProtocolEdition::Ocpp201 => {
                let update = v201::ProtectedLocalListUpdate201::from_json_bytes(native)
                    .map_err(|_| fail())?;
                values201.push(v201::ProtectedLocalListValue201 {
                    resource: resource.clone(),
                    reference: entry.reference,
                    expires_at: entry.expires_at,
                    update,
                });
                stations201.insert(entry.station_id);
            }
        }
    }
    let providers = Providers {
        v16: if values16.is_empty() {
            None
        } else {
            Some(Arc::new(
                v16::LocalAuthorizationUpdates16::new(values16).map_err(|_| fail())?,
            ))
        },
        v201: if values201.is_empty() {
            None
        } else {
            Some(Arc::new(
                v201::LocalAuthorizationUpdates201::new(values201).map_err(|_| fail())?,
            ))
        },
    };
    for (id, station) in settings {
        if stations16.contains(id) {
            station.local_authorization.clone_from(&providers.v16);
        }
        if stations201.contains(id) {
            station.local_authorization_201.clone_from(&providers.v201);
        }
    }
    Ok(providers)
}
