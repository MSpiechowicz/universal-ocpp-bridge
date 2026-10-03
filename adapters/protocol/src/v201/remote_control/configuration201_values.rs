//! Immutable, bounded secret ownership. No secret type implements Debug or Serialize.
use crate::command_registry::{
    configuration201 as registry,
    device_model201::{IdentityKey, identity_key},
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::RwLock};
use uob_application::StationCommandError;
use uob_contracts::{
    CanonicalResource, PrivilegedOcppOperation, ResourceRef, SetVariableReference201, UtcTimestamp,
    valid_configuration_reference_201,
};

pub const PROTECTED_ENTRIES_LIMIT_201: usize = 128;
pub const PROTECTED_PROFILE_BYTES_LIMIT_201: usize = 64 * 1024;
pub const PROTECTED_PROVIDER_BYTES_LIMIT_201: usize = 1024 * 1024;

pub(super) fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}
fn invalid() -> StationCommandError {
    StationCommandError::new("invalid protected configuration provisioning")
}

pub struct ProtectedVariableText201(Box<[u8]>);
impl ProtectedVariableText201 {
    /// Takes ownership, including native empty values. Errors contain no rejected content.
    /// # Errors
    /// Rejects more than 1000 Unicode characters.
    pub fn new(value: String) -> Result<Self, StationCommandError> {
        let valid = value.chars().count() <= 1000;
        let value = Self(value.into_bytes().into_boxed_slice());
        if !valid {
            return Err(invalid());
        }
        Ok(value)
    }
    pub(super) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("UTF-8 String")
    }
}
impl Drop for ProtectedVariableText201 {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

pub struct ProtectedNetworkProfile201(Box<[u8]>);
impl ProtectedNetworkProfile201 {
    /// Validates a complete pinned native connectionData object; never resolves any URL.
    /// # Errors
    /// Rejects oversized, invalid or non-native profiles without exposing payloads.
    pub fn from_json_bytes(mut bytes: Vec<u8>) -> Result<Self, StationCommandError> {
        if bytes.len() > PROTECTED_PROFILE_BYTES_LIMIT_201
            || !super::configuration201_profile_parse::safe_to_decode(&bytes)
        {
            wipe(&mut bytes);
            return Err(invalid());
        }
        let decoded = serde_json::from_slice::<Value>(&bytes);
        let valid = if let Ok(profile) = decoded {
            let mut request =
                serde_json::json!({"configurationSlot": 0, "connectionData": profile});
            let valid =
                registry::valid_schema(2, &request) && signed_fields(&request["connectionData"]);
            wipe_json(&mut request);
            valid
        } else {
            false
        };
        if !valid {
            wipe(&mut bytes);
            return Err(invalid());
        }
        Ok(Self(bytes.into_boxed_slice()))
    }
    pub(super) fn raw(&self) -> &serde_json::value::RawValue {
        serde_json::from_slice(&self.0).expect("validated native JSON")
    }
}
impl Drop for ProtectedNetworkProfile201 {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}
fn signed_fields(profile: &Value) -> bool {
    [
        profile.get("messageTimeout"),
        profile.get("securityProfile"),
        profile["apn"].get("simPin"),
    ]
    .into_iter()
    .flatten()
    .all(|value| value.as_i64().is_some_and(|n| i32::try_from(n).is_ok()))
}
pub(super) fn wipe_json(value: &mut Value) {
    match value {
        Value::String(text) => {
            let mut bytes = std::mem::take(text).into_bytes();
            wipe(&mut bytes);
        }
        Value::Array(items) => items.iter_mut().for_each(wipe_json),
        Value::Object(fields) => {
            for (key, mut value) in std::mem::take(fields) {
                let mut key = key.into_bytes();
                wipe(&mut key);
                wipe_json(&mut value);
            }
        }
        _ => {}
    }
}

pub struct ProtectedVariableValue201 {
    pub resource: ResourceRef,
    pub entry: SetVariableReference201,
    pub value: ProtectedVariableText201,
    pub expires_at: UtcTimestamp,
}
pub struct ProtectedNetworkProfileValue201 {
    pub resource: ResourceRef,
    pub configuration_slot: i32,
    pub reference: String,
    pub profile: ProtectedNetworkProfile201,
    pub expires_at: UtcTimestamp,
}
pub(super) enum ProtectedEntry {
    Variable {
        resource: ResourceRef,
        expires_at: UtcTimestamp,
        value: ProtectedVariableText201,
        identity: IdentityKey,
    },
    Network(ProtectedNetworkProfileValue201),
}
pub struct LocalConfigurationValues201 {
    entries: RwLock<BTreeMap<String, ProtectedEntry>>,
}
impl LocalConfigurationValues201 {
    /// Installs one immutable startup set. Capabilities must be independently random, not digests.
    /// # Errors
    /// Rejects excessive memory, duplicate capabilities and invalid resource/identity bindings.
    pub fn new(
        variables: Vec<ProtectedVariableValue201>,
        profiles: Vec<ProtectedNetworkProfileValue201>,
    ) -> Result<Self, StationCommandError> {
        if variables.len() + profiles.len() > PROTECTED_ENTRIES_LIMIT_201 {
            return Err(invalid());
        }
        let mut entries = BTreeMap::new();
        let mut bytes = 0usize;
        for value in variables {
            if !bounded_resource(&value.resource)
                || !registry::valid_entry(&value.resource, &value.entry)
            {
                return Err(invalid());
            }
            bytes += value.value.0.len();
            let ProtectedVariableValue201 {
                resource,
                entry,
                value,
                expires_at,
            } = value;
            let identity = identity_key(
                &entry.component,
                &entry.variable,
                entry.attribute_type.unwrap_or_default(),
            );
            if entries
                .insert(
                    entry.value_reference,
                    ProtectedEntry::Variable {
                        resource,
                        expires_at,
                        value,
                        identity,
                    },
                )
                .is_some()
            {
                return Err(invalid());
            }
        }
        for value in profiles {
            if !bounded_resource(&value.resource)
                || value.resource.resource.is_some()
                || value.resource.native_protocol_reference.is_some()
                || !valid_configuration_reference_201(&value.reference)
            {
                return Err(invalid());
            }
            bytes += value.profile.0.len();
            if entries
                .insert(value.reference.clone(), ProtectedEntry::Network(value))
                .is_some()
            {
                return Err(invalid());
            }
        }
        if bytes > PROTECTED_PROVIDER_BYTES_LIMIT_201 {
            return Err(invalid());
        }
        Ok(Self {
            entries: RwLock::new(entries),
        })
    }
    /// Linearizes removal against synchronous resolution. It cannot unsend an in-flight socket write.
    /// # Errors
    /// Poisoned provider state fails closed.
    pub fn revoke(&self, reference: &str) -> Result<(), StationCommandError> {
        self.entries
            .write()
            .map_err(|_| invalid())?
            .remove(reference);
        Ok(())
    }
    /// Non-secret, all-reference preflight for admission. The same provider must own dispatch.
    /// # Errors
    /// Rejects invalid envelopes, foreign bindings, expiry and revocation without resolving values.
    pub fn validate_operation(
        &self,
        resource: &ResourceRef,
        operation: &PrivilegedOcppOperation<Value>,
        now: UtcTimestamp,
    ) -> Result<(), StationCommandError> {
        let request = registry::validate(resource, operation).map_err(|_| invalid())?;
        self.with_entries(resource, &request, now, |_| ())
            .ok_or_else(invalid)
    }
    pub(super) fn with_entries<T>(
        &self,
        resource: &ResourceRef,
        request: &registry::Request,
        now: UtcTimestamp,
        use_entries: impl FnOnce(&BTreeMap<String, ProtectedEntry>) -> T,
    ) -> Option<T> {
        let entries = self.entries.read().ok()?;
        let valid = match request {
            registry::Request::Variables(request) => request.set_variable_data.iter().all(|entry| {
                matches!(entries.get(&entry.value_reference), Some(ProtectedEntry::Variable {
                    resource: binding, expires_at, identity, ..
                }) if binding == resource && now < *expires_at
                    && *identity == identity_key(&entry.component, &entry.variable, entry.attribute_type.unwrap_or_default()))
            }),
            registry::Request::Network(request) => matches!(entries.get(&request.profile_reference),
                Some(ProtectedEntry::Network(value)) if value.resource == *resource
                    && value.configuration_slot == request.configuration_slot && now < value.expires_at),
        };
        valid.then(|| use_entries(&entries))
    }
}

fn bounded_resource(resource: &ResourceRef) -> bool {
    if resource.bridge_id.as_str().len() > 256 || resource.station_id.as_str().len() > 256 {
        return false;
    }
    match &resource.resource {
        None => true,
        Some(CanonicalResource::Connector { connector_id }) => connector_id.as_str().len() <= 256,
        Some(CanonicalResource::Evse {
            evse_id,
            connector_id,
        }) => {
            evse_id.as_str().len() <= 256
                && connector_id
                    .as_ref()
                    .is_none_or(|id| id.as_str().len() <= 256)
        }
    }
}
