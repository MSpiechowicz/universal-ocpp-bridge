mod model;
mod offline;
mod persistence;
#[cfg(test)]
mod tests;
pub mod transport;
pub(crate) mod wire;

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ocpp_client::ocpp_types::v16::common::{
    IdTagInfoStatus, SendLocalListResponseStatus as ListStatus, UpdateType,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use zeroize::Zeroize;

pub use offline::{OfflineRecord, ReplayState};

pub const ENTRY_LIMIT: usize = 256;
pub const OFFLINE_LIMIT: usize = 128;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAuthorizationConfig {
    pub private_state_file: String,
    #[serde(default = "enabled")]
    pub list_supported: bool,
    #[serde(default = "enabled")]
    pub cache_supported: bool,
}

const fn enabled() -> bool {
    true
}

#[derive(Clone)]
pub struct LocalAuthorizationHandle(
    pub(crate) Arc<Mutex<LocalAuthorization>>,
    pub(crate) Arc<tokio::sync::Mutex<()>>,
);

impl fmt::Debug for LocalAuthorizationHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalAuthorizationHandle(<private>)")
    }
}
impl PartialEq for LocalAuthorizationHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for LocalAuthorizationHandle {}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeInfo {
    pub status: IdTagInfoStatus,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiry_date: Option<String>,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_id_tag: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeEntry {
    pub id_tag: String,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub id_tag_info: Option<NativeInfo>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeUpdate {
    pub list_version: i32,
    pub update_type: UpdateType,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub local_authorization_list: Option<Vec<NativeEntry>>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrivateState {
    format_version: u16,
    station: String,
    protocol: String,
    version: i32,
    list: BTreeMap<String, NativeEntry>,
    cache: BTreeMap<String, NativeEntry>,
    offline: Vec<OfflineRecord>,
    next_offline_id: u64,
}

pub struct LocalAuthorization {
    state: PrivateState,
    storage: Option<persistence::PrivateStorage>,
    list_supported: bool,
    cache_supported: bool,
    unavailable: bool,
    reply_pending: usize,
}

#[must_use]
pub(crate) struct NativeReplyGuard<'a> {
    handle: &'a LocalAuthorizationHandle,
}

impl Drop for NativeReplyGuard<'_> {
    fn drop(&mut self) {
        self.handle.0.lock().reply_pending -= 1;
    }
}

impl LocalAuthorizationHandle {
    /// Open or recover explicitly owned persistent OCPP 1.6 state.
    ///
    /// The private file is initialized only when genuinely absent, and this
    /// handle retains its exclusive writer lock for its lifetime.
    ///
    /// # Errors
    /// Returns a value-free error for unsafe ownership, permissions, aliases,
    /// invalid owner/version binding, malformed state, or failed durable I/O.
    pub fn open(station: &str, config: &LocalAuthorizationConfig) -> Result<Self, &'static str> {
        let storage = persistence::PrivateStorage::open(&config.private_state_file)?;
        let state = storage
            .load(station)?
            .unwrap_or_else(|| PrivateState::empty(station));
        let model = LocalAuthorization {
            state,
            storage: Some(storage),
            list_supported: config.list_supported,
            cache_supported: config.cache_supported,
            unavailable: false,
            reply_pending: 0,
        };
        model.validate()?;
        model.persist(&model.state)?;
        Ok(Self(
            Arc::new(Mutex::new(model)),
            Arc::new(tokio::sync::Mutex::new(())),
        ))
    }

    /// Construct an independent, non-durable model for native-semantic tests.
    ///
    /// This handle cannot be injected into an actual protocol connection;
    /// native accepted mutations and offline transaction records require disk.
    #[must_use]
    pub fn in_memory(station: &str) -> Self {
        Self(
            Arc::new(Mutex::new(LocalAuthorization {
                state: PrivateState::empty(station),
                storage: None,
                list_supported: true,
                cache_supported: true,
                unavailable: false,
                reply_pending: 0,
            })),
            Arc::new(tokio::sync::Mutex::new(())),
        )
    }

    pub(crate) fn unsupported(station: &str) -> Self {
        let handle = Self::in_memory(station);
        {
            let mut model = handle.0.lock();
            model.list_supported = false;
            model.cache_supported = false;
        }
        handle
    }

    pub(crate) fn has_persistence(&self) -> bool {
        self.0.lock().storage.is_some()
    }

    pub(crate) fn begin_reply(&self) -> NativeReplyGuard<'_> {
        self.0.lock().reply_pending += 1;
        NativeReplyGuard { handle: self }
    }

    pub(crate) fn settled_snapshot(&self) -> Option<serde_json::Value> {
        let model = self.0.lock();
        (model.reply_pending == 0).then(|| model.snapshot())
    }

    /// Return only safe native counters/version, or state availability failure.
    #[must_use]
    pub fn snapshot(&self) -> serde_json::Value {
        self.0.lock().snapshot()
    }

    /// Apply native Full/Differential semantics and report native status.
    #[must_use]
    pub fn update(&self, request: &NativeUpdate) -> ListStatus {
        self.0.lock().update(request)
    }

    /// Clear only the independent cache, reporting durable success.
    #[must_use]
    pub fn clear_cache(&self) -> bool {
        self.0.lock().clear_cache()
    }

    /// Evaluate native local-list priority, status, and expiry without a CSMS call.
    #[must_use]
    pub fn authorize_offline(&self, token: &str, now: OffsetDateTime) -> bool {
        self.0.lock().authorize_offline(token, now)
    }

    /// Observe central authorization; identities in an enabled list are never cached.
    ///
    /// Returns whether a connector-zero `LocalListConflict` must be reported.
    /// A retained but disabled list neither takes priority nor suppresses an enabled cache.
    ///
    /// # Errors
    /// Returns a value-free error for invalid native information, capacity,
    /// unavailable state, or a failed durable cache update.
    pub fn observe_central(&self, token: &str, info: NativeInfo) -> Result<bool, &'static str> {
        self.0.lock().observe_central(token, info)
    }
    pub(crate) fn station_matches(&self, station: &str) -> bool {
        self.0.lock().state.station == station
    }

    pub(crate) fn reload(&self) -> Result<(), &'static str> {
        let mut model = self.0.lock();
        if let Some(storage) = &model.storage {
            let recovered = storage
                .load(&model.state.station)?
                .ok_or("private_state_missing")?;
            let prior = std::mem::replace(&mut model.state, recovered);
            if let Err(error) = model.validate() {
                model.state = prior;
                return Err(error);
            }
        }
        for record in &mut model.state.offline {
            record.replay.recover_uncertain();
        }
        model.unavailable = false;
        Ok(())
    }
}

impl PrivateState {
    fn empty(station: &str) -> Self {
        Self {
            format_version: 1,
            station: station.to_owned(),
            protocol: "ocpp1.6".to_owned(),
            version: 0,
            list: BTreeMap::new(),
            cache: BTreeMap::new(),
            offline: Vec::new(),
            next_offline_id: 1,
        }
    }
}

impl Drop for NativeInfo {
    fn drop(&mut self) {
        self.expiry_date.zeroize();
        self.parent_id_tag.zeroize();
    }
}

impl Drop for NativeEntry {
    fn drop(&mut self) {
        self.id_tag.zeroize();
    }
}

impl Drop for PrivateState {
    fn drop(&mut self) {
        for (mut identity, _) in std::mem::take(&mut self.list) {
            identity.zeroize();
        }
        for (mut identity, _) in std::mem::take(&mut self.cache) {
            identity.zeroize();
        }
    }
}

pub(crate) fn non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
