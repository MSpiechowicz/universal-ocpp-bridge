//! Independent OCPP 2.0.1 station reservation model, derived from OCA 2.0.1 Part 2 §H
//! (H01–H04) and Errata v1.0 §10. It never shares bridge types, encoders or decisions.
mod capacity;
mod model;
#[cfg(test)]
mod tests;
pub(crate) mod transport;

use crate::local_authorization201::IdToken;
use crate::reservation16::persistence::PrivateStorage;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, sync::Arc};
use time::OffsetDateTime;

pub const RESERVATION_LIMIT: usize = 128;
pub const UPDATE_LIMIT: usize = 256;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation201Config {
    pub private_state_file: String,
    /// `ReservationCtrlr.Available`/`Enabled`; otherwise every request is Rejected (H01.FR.01).
    #[serde(default)]
    pub enabled: bool,
    /// `ReservationCtrlr.NonEvseSpecific` (H01.FR.18/19).
    #[serde(default)]
    pub non_evse_specific: bool,
    #[serde(default)]
    pub connector_types: Vec<ConnectorTypeDefinition>,
}

/// Physical connector type; unconfigured connectors match no requested `connectorType`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorTypeDefinition {
    pub evse: u16,
    pub connector: u16,
    #[serde(rename = "type")]
    pub connector_type: String,
}

#[derive(Clone, Debug, Default)]
pub struct Reservation201Policy {
    pub enabled: bool,
    pub non_evse_specific: bool,
    pub connector_types: Vec<ConnectorTypeDefinition>,
}

impl Reservation201Config {
    #[must_use]
    pub fn policy(&self) -> Reservation201Policy {
        Reservation201Policy {
            enabled: self.enabled,
            non_evse_specific: self.non_evse_specific,
            connector_types: self.connector_types.clone(),
        }
    }
}

/// Native `ReserveNowRequest`; raw identities stay in this private model and its file.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveNowRequest {
    pub id: i32,
    pub expiry_date_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse_id: Option<i32>,
    pub id_token: IdToken,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id_token: Option<IdToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
}

impl Drop for ReserveNowRequest {
    fn drop(&mut self) {
        if let Some(value) = &mut self.custom_data {
            crate::local_authorization201::wipe_json(value);
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum ReserveStatus {
    Accepted,
    Faulted,
    Occupied,
    Rejected,
    Unavailable,
}

/// Actual connector state; `Reserved` is only the reported view of an Available connector.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum ConnectorStatus {
    Available,
    Occupied,
    Unavailable,
    Faulted,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum UpdateStatus {
    Expired,
    Removed,
}

/// Durable outbox entry for `ReservationStatusUpdateRequest` (H04.FR.01, H01.FR.16/17).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusUpdate {
    pub reservation_id: i32,
    pub reservation_update_status: UpdateStatus,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrivateState {
    format_version: u16,
    station: String,
    protocol: String,
    evses: BTreeMap<u16, BTreeMap<u16, ConnectorStatus>>,
    reservations: BTreeMap<i32, ReserveNowRequest>,
    updates: Vec<StatusUpdate>,
    revision: u64,
}

type ConnectorKey = (u16, u16);

struct Model {
    state: PrivateState,
    storage: Option<PrivateStorage>,
    enabled: bool,
    non_evse_specific: bool,
    types: BTreeMap<ConnectorKey, String>,
    unavailable: bool,
    /// Reported connector views awaiting a `StatusNotificationRequest`; transient by design.
    statuses: Vec<(ConnectorKey, &'static str)>,
}

#[derive(Clone)]
pub struct Reservation201Handle(Arc<Mutex<Model>>);
impl fmt::Debug for Reservation201Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reservation201Handle(<private>)")
    }
}

impl Reservation201Handle {
    /// Open an exclusively owned, bounded, private native station state.
    /// # Errors
    /// Unsafe files, incompatible topology, corrupt data and failed durable writes fail closed.
    pub fn open(
        station: &str,
        evses: &[ConnectorKey],
        config: &Reservation201Config,
    ) -> Result<Self, &'static str> {
        let types = model::types(evses, &config.policy())?;
        let storage = PrivateStorage::open(&config.private_state_file)?;
        let state = storage.load()?.unwrap_or_else(|| empty(station, evses));
        model::validate(&state, station, evses)?;
        storage.commit(&state)?;
        Ok(Self(Arc::new(Mutex::new(Model {
            state,
            storage: Some(storage),
            enabled: config.enabled,
            non_evse_specific: config.non_evse_specific,
            types,
            unavailable: false,
            statuses: Vec::new(),
        }))))
    }

    /// Non-durable model for isolated native semantics tests, never a live transport fallback.
    /// # Errors
    /// Connector types must name configured connectors and pinned `ConnectorEnumType` values.
    pub fn in_memory(
        station: &str,
        evses: &[ConnectorKey],
        policy: &Reservation201Policy,
    ) -> Result<Self, &'static str> {
        Ok(Self(Arc::new(Mutex::new(Model {
            state: empty(station, evses),
            storage: None,
            enabled: policy.enabled,
            non_evse_specific: policy.non_evse_specific,
            types: model::types(evses, policy)?,
            unavailable: false,
            statuses: Vec::new(),
        }))))
    }

    /// Apply a native reservation without granting authorization.
    /// # Errors
    /// Invalid fields, capacity or failed private durability return value-free errors.
    pub fn reserve(
        &self,
        request: ReserveNowRequest,
        now: OffsetDateTime,
    ) -> Result<ReserveStatus, &'static str> {
        self.0.lock().reserve(request, now)
    }

    /// Cancel only a current reservation with this exact ID; never queues a status update.
    /// # Errors
    /// Failed expiry/cancellation durability leaves no fabricated Accepted response.
    pub fn cancel(&self, id: i32, now: OffsetDateTime) -> Result<bool, &'static str> {
        self.0.lock().cancel(id, now)
    }

    /// Inclusive expiry, applied whether or not a CSMS socket exists (H04).
    /// # Errors
    /// Failed private state writes leave expiry uncertain and fail closed.
    pub fn expire(&self, now: OffsetDateTime) -> Result<(), &'static str> {
        self.0.lock().expire(now)
    }

    /// Observe actual native connector state; Faulted/Unavailable EVSEs lose their reservations.
    /// # Errors
    /// Unknown connectors, a non-reservation use of reserved capacity and failed durability reject it.
    pub fn set_connector(
        &self,
        evse: u16,
        connector: u16,
        status: ConnectorStatus,
    ) -> Result<(), &'static str> {
        self.0.lock().set_connector(evse, connector, status)
    }

    /// Start on one connector; returns the reservation this start ends (H01.FR.15).
    /// # Errors
    /// Unauthorized identities, mismatched identities, unavailable or reserved capacity reject it.
    pub fn start(
        &self,
        evse: u16,
        connector: u16,
        identity: Option<(&IdToken, Option<&IdToken>)>,
        authorized: bool,
        now: OffsetDateTime,
    ) -> Result<Option<i32>, &'static str> {
        self.0
            .lock()
            .start(evse, connector, identity, authorized, now, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_expected(
        &self,
        evse: u16,
        connector: u16,
        identity: Option<(&IdToken, Option<&IdToken>)>,
        authorized: bool,
        now: OffsetDateTime,
        expected: Option<i32>,
    ) -> Result<Option<i32>, &'static str> {
        self.0
            .lock()
            .start(evse, connector, identity, authorized, now, expected)
    }

    /// Reported connector views queued since the last call, oldest first.
    #[must_use]
    pub fn take_statuses(&self) -> Vec<(ConnectorKey, &'static str)> {
        std::mem::take(&mut self.0.lock().statuses)
    }

    pub(crate) fn requeue_statuses(&self, mut pending: Vec<(ConnectorKey, &'static str)>) {
        let mut model = self.0.lock();
        // Newer views queued meanwhile supersede the unsent older ones.
        pending.retain(|(key, _)| !model.statuses.iter().any(|(newer, _)| newer == key));
        pending.append(&mut model.statuses);
        model.statuses = pending;
    }

    /// Durable `ReservationStatusUpdate` outbox, oldest first; entries leave only on CSMS reply.
    #[must_use]
    pub fn updates(&self) -> Vec<StatusUpdate> {
        self.0.lock().state.updates.clone()
    }

    /// Remove one delivered update after its correlated `ReservationStatusUpdateResponse`.
    /// # Errors
    /// Failed durability keeps the update for another delivery attempt.
    pub fn acknowledge(&self, update: StatusUpdate) -> Result<(), &'static str> {
        self.0.lock().acknowledge(update)
    }

    #[must_use]
    pub fn snapshot(&self) -> Value {
        let model = self.0.lock();
        json!({"stateAvailable": !model.unavailable,
            "activeReservations": model.state.reservations.len(), "revision": model.state.revision,
            "pendingUpdates": model.state.updates.len(),
            "reservations": model.state.reservations.values().map(|r| json!({
                "id": r.id, "evseId": r.evse_id, "connectorType": r.connector_type,
                "expiryDateTime": r.expiry_date_time
            })).collect::<Vec<_>>()})
    }
}

fn empty(station: &str, evses: &[ConnectorKey]) -> PrivateState {
    let mut topology: BTreeMap<u16, BTreeMap<u16, ConnectorStatus>> = BTreeMap::new();
    for (evse, connector) in evses {
        topology
            .entry(*evse)
            .or_default()
            .insert(*connector, ConnectorStatus::Available);
    }
    PrivateState {
        format_version: 1,
        station: station.to_owned(),
        protocol: "ocpp2.0.1".to_owned(),
        evses: topology,
        reservations: BTreeMap::new(),
        updates: Vec::new(),
        revision: 0,
    }
}
