use super::{
    ENTRY_LIMIT, IdTagInfoStatus, ListStatus, LocalAuthorization, NativeEntry, NativeInfo,
    NativeUpdate, OFFLINE_LIMIT, PrivateState, UpdateType,
};
use caseless::{Caseless, default_case_fold_str as fold, default_caseless_match_str};
use std::collections::HashSet;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::{Zeroize, Zeroizing};
impl LocalAuthorization {
    pub(super) fn snapshot(&self) -> serde_json::Value {
        if self.unavailable {
            return serde_json::json!({"stateAvailable":false});
        }
        serde_json::json!({"stateAvailable":true, "listVersion": if !self.list_supported { -1 } else if self.state.list.is_empty() { 0 } else { self.state.version },
            "listEntries": self.state.list.len(), "cacheEntries": self.state.cache.len(),
            "offlineRecords": self.state.offline.len(),
            "uncertainRecords": self.state.offline.iter().filter(|r| r.replay.is_uncertain()).count()})
    }

    pub(super) fn persist(&self, state: &PrivateState) -> Result<(), &'static str> {
        if let Some(storage) = &self.storage {
            storage.commit(state)?;
        }
        Ok(())
    }

    pub(super) fn commit(&mut self, next: PrivateState) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("private_state_unavailable");
        }
        if let Err(error) = self.persist(&next) {
            if error == "private_state_commit_uncertain" {
                // Replacement happened, but durability is unknown. Never continue
                // serving the previous RAM state or report a reliable native version.
                self.state = next;
                self.unavailable = true;
            }
            return Err(error);
        }
        self.state = next;
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if self.state.format_version != 1
            || self.state.protocol != "ocpp1.6"
            || self.state.list.len() > ENTRY_LIMIT
            || self.state.cache.len() > ENTRY_LIMIT
            || self.state.offline.len() > OFFLINE_LIMIT
            || self.state.version == -1
            || (!self.state.list.is_empty() && self.state.version == 0)
        {
            return Err("private_state_invalid");
        }
        for entries in [&self.state.list, &self.state.cache] {
            for (key, entry) in entries {
                if !key.chars().eq(entry.id_tag.chars().default_case_fold())
                    || !valid_entry(entry)
                    || entry.id_tag_info.is_none()
                {
                    return Err("private_state_invalid");
                }
            }
        }
        // Support flags can change across restarts, so independently retained
        // list and cache maps may overlap. Enabled-list priority is enforced
        // during authorization and central observation, not disk validation.
        self.validate_offline()
    }

    pub(super) fn update(&mut self, request: &NativeUpdate) -> ListStatus {
        if self.unavailable {
            return ListStatus::Failed;
        }
        if !self.list_supported {
            return ListStatus::NotSupported;
        }
        if matches!(request.list_version, -1 | 0) {
            return ListStatus::Failed;
        }
        if request.update_type == UpdateType::Differential
            && request.list_version <= self.state.version
        {
            return ListStatus::VersionMismatch;
        }
        let entries = request
            .local_authorization_list
            .as_deref()
            .unwrap_or_default();
        let mut identities = PrivateIdentities(HashSet::new());
        if entries.len() > ENTRY_LIMIT
            || !update_fits(request)
            || entries.iter().any(|entry| {
                !valid_entry(entry)
                    || !identities.0.insert(fold(&entry.id_tag))
                    || (request.update_type == UpdateType::Full && entry.id_tag_info.is_none())
            })
        {
            return ListStatus::Failed;
        }
        let mut next = self.state.clone();
        if request.update_type == UpdateType::Full {
            next.list.clear();
        }
        for entry in entries {
            let mut key = Zeroizing::new(fold(&entry.id_tag));
            if entry.id_tag_info.is_some() {
                next.list.insert(std::mem::take(&mut *key), entry.clone());
            } else {
                next.list.remove(key.as_str());
            }
        }
        if next.list.len() > ENTRY_LIMIT {
            return ListStatus::Failed;
        }
        next.cache.retain(|key, _| !next.list.contains_key(key));
        next.version = request.list_version;
        if self.commit(next).is_ok() {
            ListStatus::Accepted
        } else {
            ListStatus::Failed
        }
    }

    pub(super) fn clear_cache(&mut self) -> bool {
        if self.unavailable || !self.cache_supported {
            return false;
        }
        let mut next = self.state.clone();
        next.cache.clear();
        self.commit(next).is_ok()
    }

    pub(super) fn authorize_offline(&self, token: &str, now: OffsetDateTime) -> bool {
        if self.unavailable {
            return false;
        }
        let key = Zeroizing::new(fold(token));
        let entry = self
            .state
            .list
            .get(key.as_str())
            .filter(|_| self.list_supported)
            .or_else(|| {
                self.state
                    .cache
                    .get(key.as_str())
                    .filter(|_| self.cache_supported)
            });
        entry
            .and_then(|e| e.id_tag_info.as_ref())
            .is_some_and(|info| allowed(info, now))
    }

    pub(super) fn observe_central(
        &mut self,
        token: &str,
        info: NativeInfo,
    ) -> Result<bool, &'static str> {
        if self.unavailable {
            return Err("private_state_unavailable");
        }
        let mut key = Zeroizing::new(fold(token));
        if token.chars().count() > 20 || !valid_info(&info) {
            return Err("invalid_central_authorization");
        }
        if let Some(local) = self
            .state
            .list
            .get(key.as_str())
            .filter(|_| self.list_supported)
            .and_then(|entry| entry.id_tag_info.as_ref())
        {
            let now = OffsetDateTime::now_utc();
            let parents_equal = match (&local.parent_id_tag, &info.parent_id_tag) {
                (Some(left), Some(right)) => default_caseless_match_str(left, right),
                (None, None) => true,
                _ => false,
            };
            return Ok(allowed(local, now) != allowed(&info, now) || !parents_equal);
        }
        if self.cache_supported {
            if !self.state.cache.contains_key(key.as_str()) && self.state.cache.len() == ENTRY_LIMIT
            {
                return Err("authorization_cache_capacity");
            }
            let mut next = self.state.clone();
            next.cache.insert(
                std::mem::take(&mut *key),
                NativeEntry {
                    id_tag: token.to_owned(),
                    id_tag_info: Some(info),
                },
            );
            self.commit(next)?;
        }
        Ok(false)
    }
}

fn valid_entry(entry: &NativeEntry) -> bool {
    entry.id_tag.chars().count() <= 20 && entry.id_tag_info.as_ref().is_none_or(valid_info)
}

pub(super) fn valid_info(info: &NativeInfo) -> bool {
    info.parent_id_tag
        .as_ref()
        .is_none_or(|parent| parent.chars().count() <= 20)
        && info
            .expiry_date
            .as_ref()
            .is_none_or(|date| OffsetDateTime::parse(date, &Rfc3339).is_ok())
}

fn allowed(info: &NativeInfo, now: OffsetDateTime) -> bool {
    info.status == IdTagInfoStatus::Accepted
        && info.expiry_date.as_ref().is_none_or(|date| {
            OffsetDateTime::parse(date, &Rfc3339).is_ok_and(|expiry| expiry > now)
        })
}

struct PrivateIdentities(HashSet<String>);
impl Drop for PrivateIdentities {
    fn drop(&mut self) {
        for mut identity in self.0.drain() {
            identity.zeroize();
        }
    }
}

fn update_fits(request: &NativeUpdate) -> bool {
    struct Capacity(usize);
    impl std::io::Write for Capacity {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::FileTooLarge, "native update capacity")
            })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Capacity(64 * 1024), request).is_ok()
}
