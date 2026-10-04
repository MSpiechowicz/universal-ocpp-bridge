//! Private, immutable startup provisioning; never a public command or reload surface.
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    sync::Arc,
};

use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use uob_contracts::{
    ProtocolEdition, ResourceRef, SetVariableReference201, StationId, UtcTimestamp,
};
use uob_protocol_adapter::v201::remote_control::configuration201_values::{
    LocalConfigurationValues201, PROTECTED_ENTRIES_LIMIT_201, ProtectedNetworkProfile201,
    ProtectedNetworkProfileValue201, ProtectedVariableText201, ProtectedVariableValue201,
};

use super::{StationSettings, files};

pub(super) const FILE_BYTES_LIMIT: usize = 2 * 1024 * 1024;
const INVALID: &str = "charging protected configuration unavailable";

// Decoded owned strings and raw profile JSON wipe on every exit, including serde errors.
struct SecretText(files::PrivateBytes);
impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Self(files::PrivateBytes(text.into_bytes())))
    }
}
impl SecretText {
    fn take_string(&mut self) -> String {
        String::from_utf8(std::mem::take(&mut self.0.0)).expect("decoded UTF-8 string")
    }
}
pub(super) struct SecretJson(Option<Box<RawValue>>);
impl<'de> Deserialize<'de> for SecretJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<RawValue>::deserialize(deserializer).map(|raw| Self(Some(raw)))
    }
}
impl SecretJson {
    pub(super) fn take_bytes(&mut self) -> Vec<u8> {
        let raw: Box<str> = self.0.take().expect("owned raw JSON").into();
        raw.into_string().into_bytes()
    }
}
impl Drop for SecretJson {
    fn drop(&mut self) {
        if self.0.is_some() {
            files::wipe(&mut self.take_bytes());
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provisioning {
    variables: Vec<Variable>,
    network_profiles: Vec<Network>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Variable {
    resource: ResourceRef,
    entry: SetVariableReference201,
    value: SecretText,
    expires_at: UtcTimestamp,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Network {
    resource: ResourceRef,
    configuration_slot: i32,
    reference: String,
    profile: SecretJson,
    expires_at: UtcTimestamp,
}

pub(super) fn install(
    path: Option<&Path>,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
    settings: &mut BTreeMap<StationId, StationSettings>,
    seen: &mut BTreeSet<(u64, u64)>,
) -> io::Result<Option<Arc<LocalConfigurationValues201>>> {
    let provider = path
        .map(|path| load(path, resources, settings, seen))
        .transpose()?;
    if let Some(provider) = &provider {
        for station in settings.values_mut() {
            if station.control.configuration_enabled() {
                station.configuration = Some(provider.clone());
            }
        }
    }
    Ok(provider)
}

fn load(
    path: &Path,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
    settings: &BTreeMap<StationId, StationSettings>,
    seen: &mut BTreeSet<(u64, u64)>,
) -> io::Result<Arc<LocalConfigurationValues201>> {
    let fail = || io::Error::other(INVALID);
    let bytes = files::protected(path, FILE_BYTES_LIMIT, seen).map_err(|_| fail())?;
    let provisioning: Provisioning = serde_json::from_slice(&bytes.0).map_err(|_| fail())?;
    if provisioning.variables.len() + provisioning.network_profiles.len()
        > PROTECTED_ENTRIES_LIMIT_201
    {
        return Err(fail());
    }
    let allowed = |resource: &ResourceRef, network: bool| {
        let enabled = settings.get(&resource.station_id).is_some_and(|station| {
            if station.protocol != ProtocolEdition::Ocpp201 {
                return false;
            }
            if network {
                station.control.set_network_profile.enabled()
            } else {
                station.control.set_variables.enabled()
            }
        });
        let configured = resources.get(&resource.station_id).is_some_and(|entries| {
            if network {
                entries.first() == Some(resource)
            } else {
                entries.contains(resource)
            }
        });
        enabled && configured
    };
    let mut variables = Vec::with_capacity(provisioning.variables.len());
    for mut entry in provisioning.variables {
        if !allowed(&entry.resource, false) {
            return Err(fail());
        }
        let value = ProtectedVariableText201::new(entry.value.take_string()).map_err(|_| fail())?;
        variables.push(ProtectedVariableValue201 {
            resource: entry.resource,
            entry: entry.entry,
            value,
            expires_at: entry.expires_at,
        });
    }
    let mut profiles = Vec::with_capacity(provisioning.network_profiles.len());
    for mut entry in provisioning.network_profiles {
        if !allowed(&entry.resource, true) {
            return Err(fail());
        }
        let profile = ProtectedNetworkProfile201::from_json_bytes(entry.profile.take_bytes())
            .map_err(|_| fail())?;
        profiles.push(ProtectedNetworkProfileValue201 {
            resource: entry.resource,
            configuration_slot: entry.configuration_slot,
            reference: entry.reference,
            profile,
            expires_at: entry.expires_at,
        });
    }
    LocalConfigurationValues201::new(variables, profiles)
        .map(Arc::new)
        .map_err(|_| fail())
}
