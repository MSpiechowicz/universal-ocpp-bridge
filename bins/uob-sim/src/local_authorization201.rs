mod cache;
mod native;
mod native_replay;
mod persistence;
mod private_state;
use private_state::{Delivery, OfflineRecord, PrivateState, clear_entries, validate_state};
mod device_model;
mod offline;
pub(crate) mod transport;
pub(crate) mod validation;
use crate::local_authorization::LocalAuthorizationConfig;
pub use native::{AuthorizationStatus, Entry, IdToken, IdTokenInfo, TokenType, Update, UpdateType};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use time::OffsetDateTime;
use zeroize::{Zeroize, Zeroizing};

pub const ENTRY_LIMIT: usize = 256;
pub const OFFLINE_LIMIT: usize = 128;
#[derive(Clone)]
pub struct LocalAuthorization201Handle(Arc<Mutex<Model>>, Arc<tokio::sync::Mutex<()>>);
impl std::fmt::Debug for LocalAuthorization201Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalAuthorization201Handle(<private>)")
    }
}
impl PartialEq for LocalAuthorization201Handle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for LocalAuthorization201Handle {}
struct Model {
    state: PrivateState,
    storage: Option<persistence::PrivateStorage>,
    list_enabled: bool,
    cache_enabled: bool,
    unavailable: bool,
    reply_pending: usize,
}
impl LocalAuthorization201Handle {
    /// Opens exclusively owned, edition/station-bound durable private native state.
    /// # Errors
    /// Returns value-free errors for unsafe files, invalid state or failed durable I/O.
    pub fn open(station: &str, config: &LocalAuthorizationConfig) -> Result<Self, &'static str> {
        let storage = persistence::PrivateStorage::open(&config.private_state_file)?;
        let state = storage
            .load(station)?
            .unwrap_or_else(|| PrivateState::empty(station));
        validate_state(&state)?;
        storage.commit(&state)?;
        Ok(Self(
            Arc::new(Mutex::new(Model {
                state,
                storage: Some(storage),
                list_enabled: config.list_supported,
                cache_enabled: config.cache_supported,
                unavailable: false,
                reply_pending: 0,
            })),
            Arc::new(tokio::sync::Mutex::new(())),
        ))
    }
    #[must_use]
    pub fn in_memory(station: &str) -> Self {
        Self(
            Arc::new(Mutex::new(Model {
                state: PrivateState::empty(station),
                storage: None,
                list_enabled: true,
                cache_enabled: true,
                unavailable: false,
                reply_pending: 0,
            })),
            Arc::new(tokio::sync::Mutex::new(())),
        )
    }
    pub(crate) fn unsupported(station: &str) -> Self {
        let handle = Self::in_memory(station);
        handle.0.lock().list_enabled = false;
        handle.0.lock().cache_enabled = false;
        handle
    }
    pub(crate) fn has_persistence(&self) -> bool {
        self.0.lock().storage.is_some()
    }
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let model = self.0.lock();
        if model.unavailable {
            return json!({"stateAvailable":false});
        }
        json!({"stateAvailable":true,"listVersion":if model.list_enabled {model.state.version} else {0},
            "listEntries":model.state.list.len(),"cacheEntries":model.state.cache.len(),
            "offlineRecords":model.state.offline.len(),
            "uncertainRecords":model.state.offline.iter().filter(|r| r.delivery != Delivery::Pending).count()})
    }
    pub(crate) fn settled_snapshot(&self) -> Option<Value> {
        let pending = self.0.lock().reply_pending;
        (pending == 0).then(|| self.snapshot())
    }
    /// Applies a native update atomically. Invalid requests return Failed without mutation.
    #[must_use]
    pub fn update(&self, payload: &Value) -> &'static str {
        if !validation::valid_update(payload) {
            return "Failed";
        }
        let Ok(request) = Update::deserialize(payload) else {
            return "Failed";
        };
        let mut model = self.0.lock();
        if model.unavailable || !model.list_enabled || request.version_number <= 0 {
            return "Failed";
        }
        if request.update_type == UpdateType::Differential
            && request.version_number <= model.state.version
        {
            return "VersionMismatch";
        }
        if !fits_call(payload) {
            return "Failed";
        }
        let entries = request
            .local_authorization_list
            .as_deref()
            .unwrap_or_default();
        let mut identities = PrivateIdentities(BTreeSet::new());
        for entry in entries {
            let mut key = identity(&entry.id_token);
            if identities.0.contains(&key) {
                key.zeroize();
                return "Failed";
            }
            identities.0.insert(key);
        }
        let mut next = model.state.clone();
        if request.update_type == UpdateType::Full {
            clear_entries(&mut next.list);
        }
        for entry in entries {
            let mut key = identity(&entry.id_token);
            if entry.id_token_info.is_some() {
                if let Some(previous) = next.list.get_mut(&key) {
                    *previous = entry.clone();
                    key.zeroize();
                } else {
                    next.list.insert(key, entry.clone());
                }
            } else {
                if let Some((mut stored, _)) = next.list.remove_entry(&key) {
                    stored.zeroize();
                }
                key.zeroize();
            }
        }
        if next.list.len() > ENTRY_LIMIT {
            return "Failed";
        }
        next.version = request.version_number;
        if model.commit(next).is_ok() {
            "Accepted"
        } else {
            "Failed"
        }
    }
    /// `ClearCache` never changes list or offline records.
    #[must_use]
    pub fn clear_cache(&self) -> bool {
        let mut model = self.0.lock();
        if model.unavailable || !model.cache_enabled {
            return false;
        }
        let mut next = model.state.clone();
        clear_entries(&mut next.cache);
        next.cache_sequence = 0;
        model.commit(next).is_ok()
    }
    #[must_use]
    pub fn authorize_offline(&self, token: &IdToken, evse: i32, now: OffsetDateTime) -> bool {
        let mut model = self.0.lock();
        if model.unavailable {
            return false;
        }
        let key = Zeroizing::new(identity(token));
        if model.list_enabled
            && let Some(entry) = model.state.list.get(key.as_str())
        {
            return entry
                .id_token_info
                .as_ref()
                .is_some_and(|info| native::allowed(info, evse, now));
        }
        if !model.cache_enabled {
            return false;
        }
        let Some(info) = model
            .state
            .cache
            .get(key.as_str())
            .and_then(|cached| cached.entry.id_token_info.as_ref())
        else {
            return false;
        };
        if cache::expired(info, now) {
            let mut next = model.state.clone();
            if let Some((mut stored, _)) = next.cache.remove_entry(key.as_str()) {
                stored.zeroize();
            }
            let _ = model.commit(next);
            return false;
        }
        native::allowed(info, evse, now)
    }
    /// Actual native list information first, then unexpired cache (H03.FR.07).
    pub(crate) fn identity_info(
        &self,
        token: &IdToken,
        now: OffsetDateTime,
    ) -> Option<IdTokenInfo> {
        let model = self.0.lock();
        if model.unavailable {
            return None;
        }
        let key = Zeroizing::new(identity(token));
        if model.list_enabled
            && let Some(entry) = model.state.list.get(key.as_str())
        {
            return entry.id_token_info.clone();
        }
        if !model.cache_enabled {
            return None;
        }
        model
            .state
            .cache
            .get(key.as_str())
            .and_then(|cached| cached.entry.id_token_info.clone())
            .filter(|info| !cache::expired(info, now))
    }
    /// No intercepted native request is between commit and its correlated reply.
    pub(crate) fn reply_idle(&self) -> bool {
        self.0.lock().reply_pending == 0
    }
    pub(crate) fn reload(&self) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        let storage = model
            .storage
            .as_ref()
            .ok_or("persistent_native_state_required")?;
        let recovered = storage
            .load(&model.state.station)?
            .ok_or("private_state_missing")?;
        validate_state(&recovered)?;
        model.state = recovered;
        model.unavailable = false;
        Ok(())
    }
}
impl Model {
    fn commit(&mut self, next: PrivateState) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("private_state_unavailable");
        }
        if !fits_state(&next) {
            return Err("private_state_capacity");
        }
        if let Some(storage) = &self.storage
            && let Err(error) = storage.commit(&next)
        {
            if error == "private_state_commit_uncertain" {
                self.unavailable = true;
            }
            return Err(error);
        }
        self.state = next;
        Ok(())
    }
}
/// Zeroize private native JSON owned by another independent 2.0.1 model.
pub(crate) fn wipe_json(value: &mut Value) {
    native::wipe(value);
}
/// Native acceptance of actual information for this EVSE at this time.
pub(crate) fn allows(info: &IdTokenInfo, evse: i32, now: OffsetDateTime) -> bool {
    native::allowed(info, evse, now)
}
fn identity(token: &IdToken) -> String {
    let kind = match token.token_type {
        TokenType::Central => "Central",
        TokenType::Emaid => "eMAID",
        TokenType::Iso14443 => "ISO14443",
        TokenType::Iso15693 => "ISO15693",
        TokenType::KeyCode => "KeyCode",
        TokenType::Local => "Local",
        TokenType::MacAddress => "MacAddress",
        TokenType::NoAuthorization => "NoAuthorization",
    };
    let mut key = String::with_capacity(kind.len() + 1 + token.id_token.len());
    key.push_str(kind);
    key.push(':');
    key.extend(
        token
            .id_token
            .chars()
            .map(|character| character.to_ascii_lowercase()),
    );
    key
}
struct Capacity(usize);
impl std::io::Write for Capacity {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("private capacity"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn fits_state(state: &PrivateState) -> bool {
    serde_json::to_writer(Capacity(1024 * 1024), state).is_ok()
}
fn fits_call(payload: &Value) -> bool {
    serde_json::to_writer(
        Capacity(64 * 1024),
        &(
            2,
            "00000000-0000-0000-0000-000000000000",
            "SendLocalList",
            payload,
        ),
    )
    .is_ok()
}

struct PrivateIdentities(BTreeSet<String>);
impl Drop for PrivateIdentities {
    fn drop(&mut self) {
        for mut key in std::mem::take(&mut self.0) {
            key.zeroize();
        }
    }
}
