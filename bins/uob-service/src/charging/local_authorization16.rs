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
use uob_protocol_adapter::v16::remote_control::{
    LocalAuthorizationUpdates16, ProtectedLocalListUpdate16, ProtectedLocalListValue16,
};

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
) -> io::Result<Option<Arc<LocalAuthorizationUpdates16>>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let fail = || io::Error::other("charging protected local authorization unavailable");
    let bytes = files::protected(path, 2 * 1024 * 1024, seen).map_err(|_| fail())?;
    let provisioning: Provisioning = serde_json::from_slice(&bytes.0).map_err(|_| fail())?;
    if provisioning.updates.len() > 128 {
        return Err(fail());
    }
    let mut values = Vec::with_capacity(provisioning.updates.len());
    for mut entry in provisioning.updates {
        if !settings.get(&entry.station_id).is_some_and(|station| {
            station.protocol == ProtocolEdition::Ocpp16j
                && station.control.send_local_list.enabled()
        }) {
            return Err(fail());
        }
        let resource = resources
            .get(&entry.station_id)
            .and_then(|resources| resources.first())
            .ok_or_else(fail)?;
        let update = ProtectedLocalListUpdate16::from_json_bytes(entry.request.take_bytes())
            .map_err(|_| fail())?;
        values.push(ProtectedLocalListValue16 {
            resource: resource.clone(),
            reference: entry.reference,
            expires_at: entry.expires_at,
            update,
        });
    }
    let provider = Arc::new(LocalAuthorizationUpdates16::new(values).map_err(|_| fail())?);
    for station in settings.values_mut() {
        if station.control.send_local_list.enabled() {
            station.local_authorization = Some(provider.clone());
        }
    }
    Ok(Some(provider))
}
