//! Immutable owner-provisioned native requests; only one-way comparison keys reach storage.
use super::local_authorization_values::{wipe, wipe_local_authorization_json};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::RwLock};
use uob_application::{
    ReservationCandidate16, ReservationKey16, SensitiveAuthorizationToken, StationCommandError,
};
use uob_contracts::{
    ReserveNowReference16, ResourceRef, UtcTimestamp, valid_reservation_reference_16,
};

#[must_use]
pub fn reservation_key_16(token: &[u8]) -> Option<ReservationKey16> {
    let text = std::str::from_utf8(token).ok()?;
    let mut folded = caseless::default_case_fold_str(text).into_bytes();
    let mut hash = Sha256::new();
    hash.update(b"uob:reservation16:CiString:");
    hash.update(&folded);
    let key = ReservationKey16(hash.finalize().into());
    wipe(&mut folded);
    Some(key)
}
struct Provisioned {
    native: Value,
    request: ReserveNowReference16,
    expires_at: UtcTimestamp,
    revoked: bool,
    candidate: ReservationCandidate16,
}
impl Drop for Provisioned {
    fn drop(&mut self) {
        wipe_local_authorization_json(&mut self.native);
    }
}
struct Identity {
    token: SensitiveAuthorizationToken,
    parent: Option<SensitiveAuthorizationToken>,
    authorize: bool,
    policy_revision: Option<u64>,
}

pub struct ReservationValues16 {
    station: ResourceRef,
    records: RwLock<BTreeMap<String, Provisioned>>,
    identities: Vec<Identity>,
    identity_keys: BTreeMap<[u8; 32], (usize, Option<ReservationKey16>)>,
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
fn token_text(value: &Value) -> Result<&str, StationCommandError> {
    let text = value.as_str().ok_or_else(invalid)?;
    if text.is_empty() || text.chars().count() > 20 || text.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(text)
}
fn token(value: &Value) -> Result<SensitiveAuthorizationToken, StationCommandError> {
    SensitiveAuthorizationToken::new(token_text(value)?.as_bytes()).map_err(|_| invalid())
}
impl ReservationValues16 {
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
    #[allow(clippy::too_many_lines)] // One pass validates every protected field in order.
    fn parse(station: ResourceRef, value: &Value) -> Result<Self, StationCommandError> {
        if station.resource.is_some()
            || !matches!(
                station.native_protocol_reference,
                None | Some(uob_contracts::NativeProtocolReference::Ocpp16 { connector_id: 0 })
            )
        {
            return Err(invalid());
        }
        if !only(value, &["reservations", "identities"], &[]) {
            return Err(invalid());
        }
        let entries = value["reservations"].as_array().ok_or_else(invalid)?;
        let identities = value["identities"].as_array().ok_or_else(invalid)?;
        if entries.len() + identities.len() > 256 {
            return Err(invalid());
        }
        let mut records = BTreeMap::new();
        for entry in entries {
            if !only(
                entry,
                &["reference", "request", "expires_at", "revoked"],
                &[],
            ) {
                return Err(invalid());
            }
            let reference = entry["reference"]
                .as_str()
                .filter(|s| valid_reservation_reference_16(s))
                .ok_or_else(invalid)?;
            let native = &entry["request"];
            if !only(
                native,
                &["connectorId", "expiryDate", "idTag", "reservationId"],
                &["parentIdTag"],
            ) {
                return Err(invalid());
            }
            token_text(&native["idTag"])?;
            if let Some(parent) = native.get("parentIdTag") {
                token_text(parent)?;
            }
            let wrapper = ReserveNowReference16 {
                connector_id: native["connectorId"]
                    .as_u64()
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or_else(invalid)?,
                expiry_date: serde_json::from_value(native["expiryDate"].clone())
                    .map_err(|_| invalid())?,
                reservation_id: native["reservationId"]
                    .as_i64()
                    .and_then(|id| i32::try_from(id).ok())
                    .ok_or_else(invalid)?,
                reservation_reference: reference.to_owned(),
            };
            let candidate = ReservationCandidate16 {
                connector_id: wrapper.connector_id,
                expiry_date: wrapper.expiry_date,
                token_key: reservation_key_16(token_text(&native["idTag"])?.as_bytes())
                    .ok_or_else(invalid)?,
                group_key: native
                    .get("parentIdTag")
                    .map(token_text)
                    .transpose()?
                    .and_then(|parent| reservation_key_16(parent.as_bytes())),
            };
            let record = Provisioned {
                native: native.clone(),
                request: wrapper,
                candidate,
                expires_at: serde_json::from_value(entry["expires_at"].clone())
                    .map_err(|_| invalid())?,
                revoked: entry["revoked"].as_bool().ok_or_else(invalid)?,
            };
            if records.insert(reference.to_owned(), record).is_some() {
                return Err(invalid());
            }
        }
        let mut known = BTreeMap::new();
        let mut result = Vec::new();
        for entry in identities {
            if !only(
                entry,
                &["idTag"],
                &["parentIdTag", "authorize", "policy_revision"],
            ) {
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
            let identity = Identity {
                token: token(&entry["idTag"])?,
                parent: entry.get("parentIdTag").map(token).transpose()?,
                authorize,
                policy_revision,
            };
            let key =
                reservation_key_16(identity.token.expose_to_provider()).ok_or_else(invalid)?;
            let parent = identity
                .parent
                .as_ref()
                .and_then(|parent| reservation_key_16(parent.expose_to_provider()));
            if known.insert(key.0, (result.len(), parent)).is_some() {
                return Err(invalid());
            }
            result.push(identity);
        }
        Ok(Self {
            station,
            records: RwLock::new(records),
            identities: result,
            identity_keys: known,
        })
    }
    pub fn candidate(
        &self,
        resource: &ResourceRef,
        request: &ReserveNowReference16,
        now: UtcTimestamp,
    ) -> Option<ReservationCandidate16> {
        self.with_record(resource, request, now, |record| {
            Some(record.candidate.clone())
        })
    }
    pub(super) fn with_native<T>(
        &self,
        resource: &ResourceRef,
        request: &ReserveNowReference16,
        now: UtcTimestamp,
        use_native: impl FnOnce(&Value) -> Option<T>,
    ) -> Option<T> {
        self.with_record(resource, request, now, |record| use_native(&record.native))
    }
    fn with_record<T>(
        &self,
        resource: &ResourceRef,
        request: &ReserveNowReference16,
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
            || now >= request.expiry_date
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
    pub fn group_key(&self, presented: &[u8]) -> Option<ReservationKey16> {
        let key = reservation_key_16(presented)?;
        self.identity_keys.get(&key.0)?.1.clone()
    }
    /// Native wire only. Caller must not serialize or persist this material elsewhere.
    pub fn parent_for_wire(&self, presented: &[u8]) -> Option<SensitiveAuthorizationToken> {
        let key = reservation_key_16(presented)?;
        self.identities
            .get(self.identity_keys.get(&key.0)?.0)?
            .parent
            .as_ref()
            .and_then(|parent| SensitiveAuthorizationToken::new(parent.expose_to_provider()).ok())
    }
    pub fn policy_entries(
        &self,
    ) -> impl Iterator<Item = (&SensitiveAuthorizationToken, bool, u64)> {
        self.identities.iter().filter_map(|i| {
            i.policy_revision
                .or(i.authorize.then_some(1))
                .map(|revision| (&i.token, i.authorize, revision))
        })
    }
}
