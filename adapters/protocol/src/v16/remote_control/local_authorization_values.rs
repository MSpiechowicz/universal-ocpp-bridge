//! Bounded immutable protected updates. No secret-bearing type implements Debug.
use serde_json::{Value, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};
use uob_application::StationCommandError;
use uob_contracts::{
    LOCAL_AUTHORIZATION_BYTES_LIMIT_16, LOCAL_AUTHORIZATION_ENTRIES_LIMIT_16,
    LocalListUpdateType16, ResourceRef, SendLocalListReference16, UtcTimestamp,
};

pub(super) fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}
pub fn wipe_local_authorization_json(value: &mut Value) {
    match value {
        Value::String(text) => wipe(&mut std::mem::take(text).into_bytes()),
        Value::Array(items) => items.iter_mut().for_each(wipe_local_authorization_json),
        Value::Object(fields) => {
            for (key, mut value) in std::mem::take(fields) {
                wipe(&mut key.into_bytes());
                wipe_local_authorization_json(&mut value);
            }
        }
        _ => {}
    }
}
fn invalid() -> StationCommandError {
    StationCommandError::new("invalid protected local authorization provisioning")
}

pub struct ProtectedLocalListUpdate16 {
    raw: Option<Box<RawValue>>,
    pub(super) version: i32,
    pub(super) update_type: LocalListUpdateType16,
    pub(super) count: usize,
}
impl ProtectedLocalListUpdate16 {
    /// Takes ownership of an entire native request; preserves omitted versus empty lists.
    /// # Errors
    /// Rejects non-native fields, identities, duplicate casefold keys and excessive material.
    pub fn from_json_bytes(mut bytes: Vec<u8>) -> Result<Self, StationCommandError> {
        if bytes.len() > LOCAL_AUTHORIZATION_BYTES_LIMIT_16
            || !crate::v201::remote_control::configuration201_profile_parse::safe_to_decode(&bytes)
        {
            wipe(&mut bytes);
            return Err(invalid());
        }
        let decoded = serde_json::from_slice::<Value>(&bytes);
        let valid = match decoded {
            Ok(mut value) => {
                let result = validate_native(&value).and_then(|metadata| {
                    serde_json::value::to_raw_value(&value)
                        .ok()
                        .map(|canonical| (metadata, canonical))
                });
                wipe_local_authorization_json(&mut value);
                result
            }
            Err(_) => None,
        };
        wipe(&mut bytes);
        let Some(((version, update_type, count), canonical)) = valid else {
            return Err(invalid());
        };
        if canonical.get().len() > LOCAL_AUTHORIZATION_BYTES_LIMIT_16 {
            wipe_raw(canonical);
            return Err(invalid());
        }
        Ok(Self {
            raw: Some(canonical),
            version,
            update_type,
            count,
        })
    }
    pub(super) fn raw(&self) -> &RawValue {
        self.raw.as_deref().expect("owned canonical native JSON")
    }
}
impl Drop for ProtectedLocalListUpdate16 {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            wipe_raw(raw);
        }
    }
}
fn wipe_raw(raw: Box<RawValue>) {
    let text: Box<str> = raw.into();
    wipe(&mut text.into_string().into_bytes());
}
fn only(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|fields| {
        required.iter().all(|key| fields.contains_key(*key))
            && fields
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
}
fn identity(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.chars().count() <= 20)
}

struct FoldedIdentities(BTreeSet<String>);
impl Drop for FoldedIdentities {
    fn drop(&mut self) {
        for text in std::mem::take(&mut self.0) {
            wipe(&mut text.into_bytes());
        }
    }
}
fn validate_native(value: &Value) -> Option<(i32, LocalListUpdateType16, usize)> {
    if !only(
        value,
        &["listVersion", "updateType"],
        &["localAuthorizationList"],
    ) {
        return None;
    }
    let version = i32::try_from(value["listVersion"].as_i64()?).ok()?;
    if matches!(version, -1 | 0) {
        return None;
    }
    let update_type = match value["updateType"].as_str()? {
        "Full" => LocalListUpdateType16::Full,
        "Differential" => LocalListUpdateType16::Differential,
        _ => return None,
    };
    let Some(list) = value.get("localAuthorizationList") else {
        return Some((version, update_type, 0));
    };
    let list = list.as_array()?;
    if list.len() > LOCAL_AUTHORIZATION_ENTRIES_LIMIT_16 {
        return None;
    }
    let mut identities = FoldedIdentities(BTreeSet::new());
    for item in list {
        if !only(item, &["idTag"], &["idTagInfo"]) || !identity(&item["idTag"]) {
            return None;
        }
        let folded = caseless::default_case_fold_str(item["idTag"].as_str()?);
        if identities.0.contains(&folded) {
            wipe(&mut folded.into_bytes());
            return None;
        }
        identities.0.insert(folded);
        match item.get("idTagInfo") {
            None if update_type == LocalListUpdateType16::Full => return None,
            None => {}
            Some(info) => {
                if !only(info, &["status"], &["expiryDate", "parentIdTag"])
                    || !matches!(
                        info["status"].as_str(),
                        Some("Accepted" | "Blocked" | "Expired" | "Invalid" | "ConcurrentTx")
                    )
                {
                    return None;
                }
                if info
                    .get("parentIdTag")
                    .is_some_and(|parent| !identity(parent))
                {
                    return None;
                }
                if let Some(expiry) = info.get("expiryDate") {
                    <UtcTimestamp as serde::Deserialize<'_>>::deserialize(expiry).ok()?;
                }
            }
        }
    }
    Some((version, update_type, list.len()))
}

pub struct ProtectedLocalListValue16 {
    pub resource: ResourceRef,
    pub reference: String,
    pub update: ProtectedLocalListUpdate16,
    pub expires_at: UtcTimestamp,
}
pub struct LocalAuthorizationUpdates16 {
    entries: RwLock<BTreeMap<String, ProtectedLocalListValue16>>,
}
impl LocalAuthorizationUpdates16 {
    /// Installs one immutable, separately random capability set.
    /// # Errors
    /// Rejects duplicate capabilities, foreign scopes or retained material above 1 MiB.
    pub fn new(values: Vec<ProtectedLocalListValue16>) -> Result<Self, StationCommandError> {
        if values.len() > 128 {
            return Err(invalid());
        }
        let mut entries = BTreeMap::new();
        // Conservative node/lock overhead and retained capability capacity are charged too.
        // Resource identities are compacted; native material is an exact-sized box.
        let mut bytes = 4096usize;
        for mut value in values {
            if !uob_contracts::valid_local_list_reference_16(&value.reference)
                || value.resource.resource.is_some()
                || !matches!(
                    value.resource.native_protocol_reference,
                    None | Some(uob_contracts::NativeProtocolReference::Ocpp16 { connector_id: 0 })
                )
                || value.resource.bridge_id.as_str().len() > 256
                || value.resource.station_id.as_str().len() > 256
            {
                return Err(invalid());
            }
            value.resource = value.resource.clone();
            bytes += value.update.raw().get().len()
                + 2 * value.reference.capacity()
                + value.resource.bridge_id.as_str().len()
                + value.resource.station_id.as_str().len()
                + 3 * std::mem::size_of::<ProtectedLocalListValue16>()
                + 256;
            if bytes > 1024 * 1024 || entries.insert(value.reference.clone(), value).is_some() {
                return Err(invalid());
            }
        }
        Ok(Self {
            entries: RwLock::new(entries),
        })
    }
    /// # Errors
    /// Poisoned provider state fails closed. Revocation cannot unsend transmitted bytes.
    pub fn revoke(&self, reference: &str) -> Result<(), StationCommandError> {
        self.entries
            .write()
            .map_err(|_| invalid())?
            .remove(reference);
        Ok(())
    }
    /// Checks immutable scope and current expiry without disclosing native entries.
    #[must_use]
    pub fn authorized(
        &self,
        resource: &ResourceRef,
        request: &SendLocalListReference16,
        now: UtcTimestamp,
    ) -> bool {
        self.with_update(resource, request, now, |_| ()).is_some()
    }
    pub(super) fn with_update<T>(
        &self,
        resource: &ResourceRef,
        request: &SendLocalListReference16,
        now: UtcTimestamp,
        use_update: impl FnOnce(&ProtectedLocalListUpdate16) -> T,
    ) -> Option<T> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(&request.update_reference)?;
        if entry.resource != *resource
            || now >= entry.expires_at
            || entry.update.version != request.list_version
            || entry.update.update_type != request.update_type
        {
            return None;
        }
        Some(use_update(&entry.update))
    }
}
