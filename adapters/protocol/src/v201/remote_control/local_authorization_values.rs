//! Bounded immutable protected updates. No secret-bearing type implements Debug.
#[cfg(test)]
#[path = "local_authorization_values_tests.rs"]
mod tests;
use serde_json::{Value, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};
use uob_application::StationCommandError;
use uob_contracts::{
    LOCAL_AUTHORIZATION_BYTES_LIMIT_201, LOCAL_AUTHORIZATION_ENTRIES_LIMIT_201,
    LocalListUpdateType201, ResourceRef, SendLocalListReference201, UtcTimestamp,
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

pub struct ProtectedLocalListUpdate201 {
    raw: Option<Box<RawValue>>,
    pub(super) version: i32,
    pub(super) update_type: LocalListUpdateType201,
    pub(super) count: usize,
    pub(super) upsert_count: usize,
    pub(super) has_expiry: bool,
}
impl ProtectedLocalListUpdate201 {
    /// Takes ownership of an entire native request; preserves omitted versus empty lists.
    /// # Errors
    /// Rejects non-native fields, identities, duplicate casefold keys and excessive material.
    pub fn from_json_bytes(mut bytes: Vec<u8>) -> Result<Self, StationCommandError> {
        if bytes.len() > LOCAL_AUTHORIZATION_BYTES_LIMIT_201
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
        let Some(((version, update_type, count, upsert_count, has_expiry), canonical)) = valid
        else {
            return Err(invalid());
        };
        if canonical.get().len() > LOCAL_AUTHORIZATION_BYTES_LIMIT_201 {
            wipe_raw(canonical);
            return Err(invalid());
        }
        Ok(Self {
            raw: Some(canonical),
            version,
            update_type,
            count,
            upsert_count,
            has_expiry,
        })
    }
    pub(super) fn raw(&self) -> &RawValue {
        self.raw.as_deref().expect("owned canonical native JSON")
    }
}
impl Drop for ProtectedLocalListUpdate201 {
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
struct FoldedIdentities(BTreeSet<(String, String)>);
impl Drop for FoldedIdentities {
    fn drop(&mut self) {
        for (kind, text) in std::mem::take(&mut self.0) {
            wipe(&mut kind.into_bytes());
            wipe(&mut text.into_bytes());
        }
    }
}
fn valid_token(token: &Value) -> bool {
    if token["type"].as_str() == Some("NoAuthorization") && token["idToken"].as_str() != Some("") {
        return false;
    }
    let identifier = |value: &Value| {
        value.as_str().is_some_and(|text| {
            text.len() <= 36
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"*-_=:+|@.".contains(&byte))
        })
    };
    identifier(&token["idToken"])
        && token.get("additionalInfo").is_none_or(|items| {
            items.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| identifier(&item["additionalIdToken"]))
            })
        })
}
fn validate_native(value: &Value) -> Option<(i32, LocalListUpdateType201, usize, usize, bool)> {
    if !super::local_authorization_schema::valid(0, value) {
        return None;
    }
    let version = i32::try_from(value["versionNumber"].as_i64()?).ok()?;
    if version <= 0 {
        return None;
    }
    let update_type = serde::Deserialize::deserialize(&value["updateType"]).ok()?;
    let Some(list) = value.get("localAuthorizationList") else {
        return Some((version, update_type, 0, 0, false));
    };
    let list = list.as_array()?;
    if list.len() > LOCAL_AUTHORIZATION_ENTRIES_LIMIT_201 {
        return None;
    }
    let mut identities = FoldedIdentities(BTreeSet::new());
    let mut upsert_count = 0;
    let mut has_expiry = false;
    for item in list {
        let info = item.get("idTokenInfo");
        if update_type == LocalListUpdateType201::Full && info.is_none() {
            return None;
        }
        let token = &item["idToken"];
        if !valid_token(token) {
            return None;
        }
        let key = (
            token["type"].as_str()?.to_owned(),
            caseless::default_case_fold_str(token["idToken"].as_str()?),
        );
        if identities.0.contains(&key) {
            wipe(&mut key.0.into_bytes());
            wipe(&mut key.1.into_bytes());
            return None;
        }
        identities.0.insert(key);
        if let Some(info) = info {
            upsert_count += 1;
            if info
                .get("groupIdToken")
                .is_some_and(|group| !valid_token(group))
            {
                return None;
            }
            if let Some(priority) = info.get("chargingPriority") {
                let priority = priority.as_i64()?;
                if !(-9..=9).contains(&priority) {
                    return None;
                }
            }
            if let Some(ids) = info.get("evseId") {
                for id in ids.as_array()? {
                    if i32::try_from(id.as_i64()?).ok()? <= 0 {
                        return None;
                    }
                }
            }
            if let Some(expiry) = info.get("cacheExpiryDateTime") {
                has_expiry = true;
                let text = expiry.as_str()?;
                if let Some((_, fraction)) = text.split_once('.')
                    && fraction.bytes().take_while(u8::is_ascii_digit).count() > 3
                {
                    return None;
                }
                <UtcTimestamp as serde::Deserialize<'_>>::deserialize(expiry).ok()?;
            }
            for key in ["language1", "language2"] {
                if let Some(language) = info.get(key)
                    && !super::local_authorization_language::valid(language.as_str()?)
                {
                    return None;
                }
            }
            if let Some(language) = info
                .get("personalMessage")
                .and_then(|message| message.get("language"))
                && !super::local_authorization_language::valid(language.as_str()?)
            {
                return None;
            }
            if let Some(language2) = info.get("language2")
                && info.get("language1").is_none_or(|language1| {
                    language1.as_str().is_none_or(|first| {
                        language2
                            .as_str()
                            .is_none_or(|second| first.eq_ignore_ascii_case(second))
                    })
                })
            {
                return None;
            }
        }
    }
    Some((version, update_type, list.len(), upsert_count, has_expiry))
}

pub struct ProtectedLocalListValue201 {
    pub resource: ResourceRef,
    pub reference: String,
    pub update: ProtectedLocalListUpdate201,
    pub expires_at: UtcTimestamp,
}
pub struct LocalAuthorizationUpdates201 {
    entries: RwLock<BTreeMap<String, ProtectedLocalListValue201>>,
}
impl LocalAuthorizationUpdates201 {
    /// Installs one immutable, separately random capability set.
    /// # Errors
    /// Rejects duplicate capabilities, foreign scopes or retained material above 1 MiB.
    pub fn new(values: Vec<ProtectedLocalListValue201>) -> Result<Self, StationCommandError> {
        if values.len() > 128 {
            return Err(invalid());
        }
        let mut entries = BTreeMap::new();
        // Conservative node/lock overhead and retained capability capacity are charged too.
        // Resource identities are compacted; native material is an exact-sized box.
        let mut bytes = 4096usize;
        for mut value in values {
            if !uob_contracts::valid_local_list_reference_201(&value.reference)
                || value.resource.resource.is_some()
                || !matches!(
                    value.resource.native_protocol_reference,
                    None | Some(uob_contracts::NativeProtocolReference::Ocpp201 {
                        evse_id: 0,
                        connector_id: None
                    })
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
                + 3 * std::mem::size_of::<ProtectedLocalListValue201>()
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
        request: &SendLocalListReference201,
        now: UtcTimestamp,
    ) -> bool {
        self.with_update(resource, request, now, |_| ()).is_some()
    }
    pub(super) fn with_update<T>(
        &self,
        resource: &ResourceRef,
        request: &SendLocalListReference201,
        now: UtcTimestamp,
        use_update: impl FnOnce(&ProtectedLocalListUpdate201) -> T,
    ) -> Option<T> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(&request.update_reference)?;
        if entry.resource != *resource
            || now >= entry.expires_at
            || entry.update.version != request.version_number
            || entry.update.update_type != request.update_type
        {
            return None;
        }
        Some(use_update(&entry.update))
    }
}
