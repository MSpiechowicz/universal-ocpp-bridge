//! Independent OCPP 1.6 station model, derived from OCA Edition 2 §5.13.
mod model;
mod persistence;
#[cfg(test)]
mod tests;
pub(crate) mod transport;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, sync::Arc};
use time::OffsetDateTime;
use zeroize::Zeroize;

pub const RESERVATION_LIMIT: usize = 128;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationConfig {
    pub private_state_file: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub reserve_connector_zero_supported: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveRequest {
    pub connector_id: u32,
    pub expiry_date: String,
    pub id_tag: String,
    pub reservation_id: i32,
    #[serde(
        default,
        deserialize_with = "non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_id_tag: Option<String>,
}

impl Drop for ReserveRequest {
    fn drop(&mut self) {
        self.id_tag.zeroize();
        if let Some(parent) = &mut self.parent_id_tag {
            parent.zeroize();
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum ConnectorState {
    Available,
    Occupied,
    Faulted,
    Unavailable,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrivateState {
    format_version: u16,
    station: String,
    connectors: BTreeMap<u32, ConnectorState>,
    station_state: ConnectorState,
    reservations: BTreeMap<i32, ReserveRequest>,
    revision: u64,
}

struct Model {
    state: PrivateState,
    storage: Option<persistence::PrivateStorage>,
    enabled: bool,
    zero_supported: bool,
    unavailable: bool,
}

#[derive(Clone)]
pub struct ReservationHandle(Arc<Mutex<Model>>);
impl fmt::Debug for ReservationHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReservationHandle(<private>)")
    }
}

impl ReservationHandle {
    /// Open an exclusively owned, bounded, private native station state.
    /// # Errors
    /// Unsafe files, incompatible topology, corrupt data and failed durable writes fail closed.
    pub fn open(
        station: &str,
        connectors: &[u16],
        config: &ReservationConfig,
    ) -> Result<Self, &'static str> {
        let storage = persistence::PrivateStorage::open(&config.private_state_file)?;
        let state = storage
            .load()?
            .unwrap_or_else(|| empty(station, connectors));
        model::validate(&state, station, connectors)?;
        storage.commit(&state)?;
        Ok(Self(Arc::new(Mutex::new(Model {
            state,
            storage: Some(storage),
            enabled: config.enabled,
            zero_supported: config.reserve_connector_zero_supported,
            unavailable: false,
        }))))
    }

    /// Non-durable model for isolated native semantics tests, never a live transport fallback.
    #[must_use]
    pub fn in_memory(
        station: &str,
        connectors: &[u16],
        enabled: bool,
        zero_supported: bool,
    ) -> Self {
        Self(Arc::new(Mutex::new(Model {
            state: empty(station, connectors),
            storage: None,
            enabled,
            zero_supported,
            unavailable: false,
        })))
    }

    /// Apply a native reservation without granting authorization.
    /// # Errors
    /// Invalid fields, revision capacity or failed private durability return value-free errors.
    pub fn reserve(
        &self,
        request: ReserveRequest,
        now: OffsetDateTime,
    ) -> Result<ReserveStatus, &'static str> {
        self.0.lock().reserve(request, now)
    }

    /// Cancel only a current native reservation with this exact signed ID.
    /// # Errors
    /// Failed expiry/cancellation durability leaves no fabricated Accepted response.
    pub fn cancel(&self, id: i32, now: OffsetDateTime) -> Result<bool, &'static str> {
        let mut model = self.0.lock();
        model.expire(now)?;
        let mut next = model.state.clone();
        if next.reservations.remove(&id).is_none() {
            return Ok(false);
        }
        model.commit(next)?;
        Ok(true)
    }

    /// Return connector IDs whose exact reservations expired; Any remains unbound.
    /// # Errors
    /// Failed private state writes leave expiry uncertain and fail closed.
    pub fn expire(&self, now: OffsetDateTime) -> Result<Vec<u32>, &'static str> {
        self.0.lock().expire(now)
    }

    /// Observe actual native availability, occupancy or failure.
    /// # Errors
    /// Unknown connectors, exhausted Any capacity and failed durability reject the transition.
    pub fn set_connector(&self, id: u32, state: ConnectorState) -> Result<(), &'static str> {
        self.0.lock().set_connector(id, state)
    }

    /// Group matching requires actual native identity facts AND explicit local authorization.
    /// # Errors
    /// Denied authorization, mismatched identity, occupied scope or failed durability reject the start.
    pub fn start(
        &self,
        connector: u32,
        token: &str,
        parent: Option<&str>,
        authorized: bool,
        now: OffsetDateTime,
    ) -> Result<Option<i32>, &'static str> {
        self.0
            .lock()
            .start(connector, token, parent, authorized, now, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_expected(
        &self,
        connector: u32,
        token: &str,
        parent: Option<&str>,
        authorized: bool,
        now: OffsetDateTime,
        expected: Option<i32>,
    ) -> Result<Option<i32>, &'static str> {
        self.0
            .lock()
            .start(connector, token, parent, authorized, now, expected)
    }

    #[must_use]
    pub fn snapshot(&self) -> serde_json::Value {
        let model = self.0.lock();
        serde_json::json!({"stateAvailable": !model.unavailable,
            "activeReservations":model.state.reservations.len(), "revision":model.state.revision,
            "reservations":model.state.reservations.values().map(|r| serde_json::json!({
                "reservationId":r.reservation_id,"connectorId":r.connector_id,"expiryDate":r.expiry_date
            })).collect::<Vec<_>>()})
    }
}

fn empty(station: &str, connectors: &[u16]) -> PrivateState {
    PrivateState {
        format_version: 1,
        station: station.to_owned(),
        connectors: connectors
            .iter()
            .map(|id| (u32::from(*id), ConnectorState::Available))
            .collect(),
        station_state: ConnectorState::Available,
        reservations: BTreeMap::new(),
        revision: 0,
    }
}

fn non_null<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}
