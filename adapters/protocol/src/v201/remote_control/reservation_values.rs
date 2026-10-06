//! Immutable owner-provisioned native 2.0.1 requests; only one-way comparison keys reach storage.
#[cfg(test)]
#[path = "reservation_values_tests.rs"]
mod tests;
use super::local_authorization_values::{wipe, wipe_local_authorization_json};
use crate::command_registry::reservation201::valid_native;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::RwLock};
use uob_application::{
    ReservationCandidate201, ReservationKey201, StationCommandError,
    charging_identity::{ChargingTokenKind, PresentedChargingIdentity},
};
use uob_contracts::{
    ReservationConnectorType201, ReserveNowReference201, ResourceRef, UtcTimestamp,
    valid_reservation_reference_201,
};

/// `IdTokenType` comparison key: the native type plus the case-folded idToken (H03.FR.01).
#[must_use]
pub fn reservation_key_201(kind: &str, token: &str) -> Option<ReservationKey201> {
    if token.is_empty() || token.chars().count() > 36 || kind.is_empty() {
        return None;
    }
    let mut folded = caseless::default_case_fold_str(token).into_bytes();
    let mut hash = Sha256::new();
    hash.update(b"uob:reservation201:IdToken:");
    hash.update(kind.as_bytes());
    hash.update(b"\0");
    hash.update(&folded);
    let key = ReservationKey201(hash.finalize().into());
    wipe(&mut folded);
    Some(key)
}
fn token_key(token: &Value) -> Option<ReservationKey201> {
    reservation_key_201(token["type"].as_str()?, token["idToken"].as_str()?)
}
struct Provisioned {
    native: Value,
    request: ReserveNowReference201,
    expires_at: UtcTimestamp,
    revoked: bool,
    candidate: ReservationCandidate201,
}
impl Drop for Provisioned {
    fn drop(&mut self) {
        wipe_local_authorization_json(&mut self.native);
    }
}
struct Identity {
    token: Value,
    authorize: bool,
    policy_revision: Option<u64>,
}
impl Drop for Identity {
    fn drop(&mut self) {
        wipe_local_authorization_json(&mut self.token);
    }
}

pub struct ReservationValues201 {
    station: ResourceRef,
    records: RwLock<BTreeMap<String, Provisioned>>,
    identities: Vec<Identity>,
    identity_groups: BTreeMap<[u8; 32], Option<ReservationKey201>>,
}
fn invalid() -> StationCommandError {
    StationCommandError::new("invalid protected reservation provisioning")
}
fn only(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|o| {
        required.iter().all(|k| o.contains_key(*k))
            && o.keys()
                .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    })
}
/// Exact `IdTokenType`; vendor `customData` is rejected rather than forwarded unseen.
fn valid_token(token: &Value) -> bool {
    let identifier = |value: &Value| {
        value.as_str().is_some_and(|text| {
            !text.is_empty()
                && text.len() <= 36
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"*-_=:+|@.".contains(&byte))
        })
    };
    only(token, &["idToken", "type"], &["additionalInfo"])
        && !matches!(token["type"].as_str(), Some("NoAuthorization") | None)
        && identifier(&token["idToken"])
        && token.get("additionalInfo").is_none_or(|items| {
            items.as_array().is_some_and(|items| {
                !items.is_empty()
                    && items.iter().all(|item| {
                        only(item, &["additionalIdToken", "type"], &[])
                            && identifier(&item["additionalIdToken"])
                            && item["type"].as_str().is_some_and(|t| t.len() <= 50)
                    })
            })
        })
}
fn kind(token: &Value) -> Option<ChargingTokenKind> {
    Some(match token["type"].as_str()? {
        "Central" => ChargingTokenKind::Central,
        "ISO14443" => ChargingTokenKind::Iso14443,
        "ISO15693" => ChargingTokenKind::Iso15693,
        "KeyCode" => ChargingTokenKind::KeyCode,
        "Local" => ChargingTokenKind::Local,
        "MacAddress" => ChargingTokenKind::MacAddress,
        _ => return None,
    })
}
impl ReservationValues201 {
    /// Takes ownership of protected per-station reservation values and wipes rejected input.
    /// # Errors
    /// Rejects oversized, unsafe, malformed or duplicate entries without exposing their contents.
    pub fn from_json_bytes(
        station: ResourceRef,
        mut bytes: Vec<u8>,
    ) -> Result<Self, StationCommandError> {
        if bytes.len() > 65_536
            || !crate::v201::remote_control::configuration201_profile_parse::safe_to_decode(&bytes)
        {
            wipe(&mut bytes);
            return Err(invalid());
        }
        let decoded = serde_json::from_slice::<Value>(&bytes);
        wipe(&mut bytes);
        let mut value = decoded.map_err(|_| invalid())?;
        let result = Self::parse(station, &value);
        wipe_local_authorization_json(&mut value);
        result
    }
    fn parse(station: ResourceRef, value: &Value) -> Result<Self, StationCommandError> {
        if !crate::command_registry::reservation201::station_scope(&station)
            || !only(value, &["reservations", "identities"], &[])
        {
            return Err(invalid());
        }
        let entries = value["reservations"].as_array().ok_or_else(invalid)?;
        let identities = value["identities"].as_array().ok_or_else(invalid)?;
        if entries.len() + identities.len() > 256 {
            return Err(invalid());
        }
        let mut records = BTreeMap::new();
        for entry in entries {
            let (reference, record) = Self::reservation(entry)?;
            if records.insert(reference, record).is_some() {
                return Err(invalid());
            }
        }
        let mut groups = BTreeMap::new();
        let mut result = Vec::new();
        for entry in identities {
            if !only(
                entry,
                &["idToken"],
                &["groupIdToken", "authorize", "policy_revision"],
            ) || !valid_token(&entry["idToken"])
                || entry.get("groupIdToken").is_some_and(|g| !valid_token(g))
            {
                return Err(invalid());
            }
            let authorize = match entry.get("authorize") {
                Some(value) => value.as_bool().ok_or_else(invalid)?,
                None => false,
            };
            let policy_revision = entry
                .get("policy_revision")
                .map(|v| {
                    v.as_u64()
                        .filter(|r| *r > 0 && i64::try_from(*r).is_ok())
                        .ok_or_else(invalid)
                })
                .transpose()?;
            if (authorize || policy_revision.is_some()) && kind(&entry["idToken"]).is_none() {
                return Err(invalid());
            }
            let key = token_key(&entry["idToken"]).ok_or_else(invalid)?;
            let group = entry.get("groupIdToken").and_then(token_key);
            if groups.insert(key.0, group).is_some() {
                return Err(invalid());
            }
            result.push(Identity {
                token: entry["idToken"].clone(),
                authorize,
                policy_revision,
            });
        }
        Ok(Self {
            station,
            records: RwLock::new(records),
            identities: result,
            identity_groups: groups,
        })
    }
    fn reservation(entry: &Value) -> Result<(String, Provisioned), StationCommandError> {
        if !only(
            entry,
            &["reference", "request", "expires_at", "revoked"],
            &[],
        ) {
            return Err(invalid());
        }
        let reference = entry["reference"]
            .as_str()
            .filter(|s| valid_reservation_reference_201(s))
            .ok_or_else(invalid)?;
        let native = &entry["request"];
        if !valid_native(0, native)
            || !only(
                native,
                &["id", "expiryDateTime", "idToken"],
                &["evseId", "connectorType", "groupIdToken"],
            )
            || !valid_token(&native["idToken"])
            || native.get("groupIdToken").is_some_and(|g| !valid_token(g))
        {
            return Err(invalid());
        }
        let evse_id = native
            .get("evseId")
            .map(|id| {
                id.as_u64()
                    .filter(|id| *id > 0)
                    .and_then(|id| u32::try_from(id).ok())
                    .filter(|id| i32::try_from(*id).is_ok())
                    .ok_or_else(invalid)
            })
            .transpose()?;
        let connector_type: Option<ReservationConnectorType201> = native
            .get("connectorType")
            .map(|kind| serde_json::from_value(kind.clone()).map_err(|_| invalid()))
            .transpose()?;
        let wrapper = ReserveNowReference201 {
            id: native["id"]
                .as_i64()
                .and_then(|id| i32::try_from(id).ok())
                .ok_or_else(invalid)?,
            expiry_date_time: serde_json::from_value(native["expiryDateTime"].clone())
                .map_err(|_| invalid())?,
            evse_id,
            connector_type,
            reservation_reference: reference.to_owned(),
        };
        let candidate = ReservationCandidate201 {
            evse_id,
            connector_type,
            expiry_date_time: wrapper.expiry_date_time,
            token_key: token_key(&native["idToken"]).ok_or_else(invalid)?,
            group_key: native
                .get("groupIdToken")
                .map(|group| token_key(group).ok_or_else(invalid))
                .transpose()?,
        };
        Ok((
            reference.to_owned(),
            Provisioned {
                native: native.clone(),
                request: wrapper,
                candidate,
                expires_at: serde_json::from_value(entry["expires_at"].clone())
                    .map_err(|_| invalid())?,
                revoked: entry["revoked"].as_bool().ok_or_else(invalid)?,
            },
        ))
    }
    pub fn candidate(
        &self,
        resource: &ResourceRef,
        request: &ReserveNowReference201,
        now: UtcTimestamp,
    ) -> Option<ReservationCandidate201> {
        self.with_record(resource, request, now, |record| {
            Some(record.candidate.clone())
        })
    }
    pub(super) fn with_native<T>(
        &self,
        resource: &ResourceRef,
        request: &ReserveNowReference201,
        now: UtcTimestamp,
        use_native: impl FnOnce(&Value) -> Option<T>,
    ) -> Option<T> {
        self.with_record(resource, request, now, |record| use_native(&record.native))
    }
    fn with_record<T>(
        &self,
        resource: &ResourceRef,
        request: &ReserveNowReference201,
        now: UtcTimestamp,
        use_record: impl FnOnce(&Provisioned) -> Option<T>,
    ) -> Option<T> {
        if resource.bridge_id != self.station.bridge_id
            || resource.station_id != self.station.station_id
        {
            return None;
        }
        let records = self.records.read().ok()?;
        let record = records.get(&request.reservation_reference)?;
        if record.revoked
            || record.request != *request
            || now >= record.expires_at
            || now >= request.expiry_date_time
        {
            return None;
        }
        use_record(record)
    }
    pub fn revoke(&self, reference: &str) -> bool {
        self.records
            .write()
            .ok()
            .and_then(|mut records| records.get_mut(reference).map(|r| r.revoked = true))
            .is_some()
    }
    /// Trusted group of a presented token, from the owner's identity list only.
    #[must_use]
    pub fn group_key(&self, presented: &ReservationKey201) -> Option<ReservationKey201> {
        self.identity_groups.get(&presented.0)?.clone()
    }
    /// Identities whose explicit local policy must be provisioned; never persisted here.
    pub fn policy_entries(&self) -> impl Iterator<Item = (PresentedChargingIdentity, bool, u64)> {
        self.identities.iter().filter_map(|identity| {
            let revision = identity
                .policy_revision
                .or(identity.authorize.then_some(1))?;
            Some((
                PresentedChargingIdentity {
                    token: identity.token["idToken"].as_str()?.to_owned(),
                    kind: kind(&identity.token)?,
                    additional: vec![],
                    certificate: None,
                    certificate_hashes: vec![],
                },
                identity.authorize,
                revision,
            ))
        })
    }
}
