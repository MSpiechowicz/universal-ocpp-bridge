use super::{ENTRY_LIMIT, Entry, OFFLINE_LIMIT, fits_state, identity, native, validation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum Delivery {
    Pending,
    Sending,
    Uncertain,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OfflineRecord {
    pub(super) payload: Value,
    pub(super) delivery: Delivery,
}
impl Drop for OfflineRecord {
    fn drop(&mut self) {
        native::wipe(&mut self.payload);
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CacheEntry {
    pub(super) entry: Entry,
    pub(super) received: u64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateState {
    pub(super) format_version: u16,
    pub(super) station: String,
    pub(super) protocol: String,
    pub(super) version: i32,
    pub(super) list: BTreeMap<String, Entry>,
    pub(super) cache: BTreeMap<String, CacheEntry>,
    pub(super) cache_sequence: u64,
    pub(super) offline: Vec<OfflineRecord>,
    pub(super) active: BTreeMap<String, Value>,
}
impl Drop for PrivateState {
    fn drop(&mut self) {
        for (mut key, mut value) in std::mem::take(&mut self.active) {
            key.zeroize();
            native::wipe(&mut value);
        }
        clear_entries(&mut self.list);
        clear_entries(&mut self.cache);
    }
}
impl PrivateState {
    pub(super) fn empty(station: &str) -> Self {
        Self {
            format_version: 1,
            station: station.to_owned(),
            protocol: "ocpp2.0.1".to_owned(),
            version: 0,
            list: BTreeMap::new(),
            cache: BTreeMap::new(),
            cache_sequence: 0,
            offline: Vec::new(),
            active: BTreeMap::new(),
        }
    }
}
pub(super) fn clear_entries<T>(entries: &mut BTreeMap<String, T>) {
    for mut key in std::mem::take(entries).into_keys() {
        key.zeroize();
    }
}
pub(super) fn validate_state(state: &PrivateState) -> Result<(), &'static str> {
    if state.version < 0
        || state.list.len() > ENTRY_LIMIT
        || state.cache.len() > ENTRY_LIMIT
        || (!state.list.is_empty() && state.version == 0)
        || state.offline.len() > OFFLINE_LIMIT
        || state.active.len() > OFFLINE_LIMIT
        || !fits_state(state)
    {
        return Err("private_state_invalid");
    }
    let mut received = BTreeSet::new();
    if state.cache.values().any(|entry| {
        entry.received == 0
            || entry.received > state.cache_sequence
            || !received.insert(entry.received)
    }) {
        return Err("private_state_invalid");
    }
    for (key, entry) in state
        .list
        .iter()
        .chain(state.cache.iter().map(|(key, cached)| (key, &cached.entry)))
    {
        let value = serde_json::to_value(entry).map_err(|_| "private_state_invalid")?;
        let update = native::PrivateJson(
            json!({"versionNumber":1,"updateType":"Full","localAuthorizationList":[value]}),
        );
        let canonical = Zeroizing::new(identity(&entry.id_token));
        if key.as_str() != canonical.as_str()
            || entry.id_token_info.is_none()
            || !validation::valid_update(&update.0)
        {
            return Err("private_state_invalid");
        }
    }
    let mut active_evses = BTreeSet::new();
    for (id, payload) in &state.active {
        if !validation::valid_event(payload)
            || payload["eventType"] != "Started"
            || payload["offline"] != true
            || payload["seqNo"] != 0
            || payload
                .pointer("/transactionInfo/transactionId")
                .and_then(Value::as_str)
                != Some(id.as_str())
            || !active_evses.insert(
                payload
                    .pointer("/evse/id")
                    .and_then(Value::as_i64)
                    .ok_or("private_state_invalid")?,
            )
        {
            return Err("private_state_invalid");
        }
    }
    for record in &state.offline {
        let payload = &record.payload;
        if !validation::valid_event(payload)
            || payload["offline"] != true
            || !matches!(payload["eventType"].as_str(), Some("Started" | "Ended"))
        {
            return Err("private_state_invalid");
        }
    }
    Ok(())
}
