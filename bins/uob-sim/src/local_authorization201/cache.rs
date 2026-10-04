use super::private_state::CacheEntry;
use super::{
    ENTRY_LIMIT, Entry, IdToken, IdTokenInfo, LocalAuthorization201Handle, identity, native,
    validation,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::{Zeroize, Zeroizing};

pub(super) fn expired(info: &IdTokenInfo, now: OffsetDateTime) -> bool {
    info.cache_expiry_date_time
        .as_deref()
        .is_some_and(|date| OffsetDateTime::parse(date, &Rfc3339).is_ok_and(|expiry| expiry <= now))
}

impl LocalAuthorization201Handle {
    /// C10 caches the latest native CSMS information regardless of status or list
    /// membership. It never changes the separate Local Authorization List.
    /// # Errors
    /// Returns value-free errors for invalid information or durable failure.
    pub fn observe_central(&self, token: IdToken, info: IdTokenInfo) -> Result<(), &'static str> {
        let token_json =
            native::PrivateJson(serde_json::to_value(&token).map_err(|_| "invalid_native_token")?);
        let info_json =
            native::PrivateJson(serde_json::to_value(&info).map_err(|_| "invalid_native_info")?);
        if !validation::valid_token(&token_json.0) || !validation::valid_info(&info_json.0) {
            return Err("invalid_native_info");
        }
        let mut model = self.0.lock();
        if model.unavailable {
            return Err("private_state_unavailable");
        }
        if !model.cache_enabled {
            return Ok(());
        }
        let mut key = Zeroizing::new(identity(&token));
        let mut next = model.state.clone();
        if expired(&info, OffsetDateTime::now_utc()) {
            if let Some((mut stored, _)) = next.cache.remove_entry(key.as_str()) {
                stored.zeroize();
            }
            return persist_received(&mut model, next);
        }
        let Some(sequence) = next.cache_sequence.checked_add(1) else {
            model.unavailable = true;
            return Err("authorization_cache_order");
        };
        next.cache_sequence = sequence;
        let entry = CacheEntry {
            entry: Entry {
                id_token: token,
                id_token_info: Some(info),
                custom_data: None,
            },
            received: next.cache_sequence,
        };
        if let Some(previous) = next.cache.get_mut(key.as_str()) {
            *previous = entry;
        } else {
            if next.cache.len() == ENTRY_LIMIT {
                let oldest = Zeroizing::new(
                    next.cache
                        .iter()
                        .min_by_key(|(_, entry)| entry.received)
                        .map(|(key, _)| key.clone())
                        .ok_or("authorization_cache_order")?,
                );
                if let Some((mut stored, _)) = next.cache.remove_entry(oldest.as_str()) {
                    stored.zeroize();
                }
            }
            next.cache.insert(std::mem::take(&mut *key), entry);
        }
        persist_received(&mut model, next)
    }
}

fn persist_received(
    model: &mut super::Model,
    next: super::PrivateState,
) -> Result<(), &'static str> {
    let result = model.commit(next);
    // A real newer denial must never leave an older cached acceptance usable
    // when latest information could not be durably retained.
    if result.is_err() {
        model.unavailable = true;
    }
    result
}
