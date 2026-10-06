use super::{
    ConnectorState, Model, PrivateState, RESERVATION_LIMIT, ReserveRequest, ReserveStatus,
};
use caseless::default_caseless_match_str;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) fn validate(
    state: &PrivateState,
    station: &str,
    connectors: &[u16],
) -> Result<(), &'static str> {
    if state.format_version != 1
        || state.station != station
        || state.reservations.len() > RESERVATION_LIMIT
        || state.connectors.len() != connectors.len()
        || connectors
            .iter()
            .any(|id| *id == 0 || !state.connectors.contains_key(&u32::from(*id)))
        || state.reservations.iter().any(|(id, r)| {
            *id != r.reservation_id
                || !valid(r)
                || (r.connector_id != 0 && !state.connectors.contains_key(&r.connector_id))
        })
    {
        return Err("reservation_state_invalid");
    }
    Ok(())
}

pub(super) fn valid(r: &ReserveRequest) -> bool {
    r.id_tag.chars().count() <= 20
        && r.parent_id_tag
            .as_ref()
            .is_none_or(|p| p.chars().count() <= 20)
        && OffsetDateTime::parse(&r.expiry_date, &Rfc3339).is_ok()
}

impl Model {
    pub(super) fn commit(&mut self, mut next: PrivateState) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("reservation_state_unavailable");
        }
        next.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or("reservation_revision_capacity")?;
        if let Some(storage) = &self.storage
            && let Err(error) = storage.commit(&next)
        {
            if error == "reservation_commit_uncertain" {
                self.state = next;
                self.unavailable = true;
            }
            return Err(error);
        }
        self.state = next;
        Ok(())
    }

    pub(super) fn expire(&mut self, now: OffsetDateTime) -> Result<Vec<u32>, &'static str> {
        if self.unavailable {
            return Err("reservation_state_unavailable");
        }
        let expired: Vec<_> = self
            .state
            .reservations
            .values()
            .filter(|r| {
                OffsetDateTime::parse(&r.expiry_date, &Rfc3339).is_ok_and(|expiry| expiry <= now)
            })
            .map(|r| (r.reservation_id, r.connector_id))
            .collect();
        if expired.is_empty() {
            return Ok(Vec::new());
        }
        let mut next = self.state.clone();
        for (id, _) in &expired {
            next.reservations.remove(id);
        }
        self.commit(next)?;
        Ok(expired
            .into_iter()
            .filter_map(|(_, connector)| (connector != 0).then_some(connector))
            .collect())
    }

    pub(super) fn reserve(
        &mut self,
        request: ReserveRequest,
        now: OffsetDateTime,
    ) -> Result<ReserveStatus, &'static str> {
        if !valid(&request) {
            return Err("reservation_request_invalid");
        }
        self.expire(now)?;
        if !self.enabled
            || (request.connector_id == 0 && !self.zero_supported)
            || OffsetDateTime::parse(&request.expiry_date, &Rfc3339)
                .is_ok_and(|expiry| expiry <= now)
        {
            return Ok(ReserveStatus::Rejected);
        }
        let state = if request.connector_id == 0 {
            self.state.station_state
        } else {
            let Some(state) = self.state.connectors.get(&request.connector_id) else {
                return Ok(ReserveStatus::Rejected);
            };
            if self.state.station_state == ConnectorState::Available {
                *state
            } else {
                self.state.station_state
            }
        };
        match state {
            ConnectorState::Faulted => return Ok(ReserveStatus::Faulted),
            ConnectorState::Unavailable => return Ok(ReserveStatus::Unavailable),
            ConnectorState::Occupied => return Ok(ReserveStatus::Occupied),
            ConnectorState::Available => {}
        }
        let mut next = self.state.clone();
        // A rejected replacement must leave the old reservation intact.
        next.reservations.remove(&request.reservation_id);
        if next.reservations.len() == RESERVATION_LIMIT {
            return Ok(ReserveStatus::Rejected);
        }
        if request.connector_id != 0
            && next
                .reservations
                .values()
                .any(|r| r.connector_id == request.connector_id)
        {
            return Ok(ReserveStatus::Occupied);
        }
        let id = request.reservation_id;
        next.reservations.insert(id, request);
        if !capacity(&next) {
            let usable = next
                .connectors
                .values()
                .any(|s| *s == ConnectorState::Available);
            if !usable
                && next
                    .connectors
                    .values()
                    .all(|s| *s == ConnectorState::Faulted)
            {
                return Ok(ReserveStatus::Faulted);
            }
            if !usable
                && next
                    .connectors
                    .values()
                    .all(|s| matches!(s, ConnectorState::Faulted | ConnectorState::Unavailable))
            {
                return Ok(ReserveStatus::Unavailable);
            }
            return Ok(ReserveStatus::Occupied);
        }
        self.commit(next)?;
        Ok(ReserveStatus::Accepted)
    }

    pub(super) fn set_connector(
        &mut self,
        id: u32,
        state: ConnectorState,
    ) -> Result<(), &'static str> {
        let mut next = self.state.clone();
        if id == 0 {
            next.station_state = state;
        } else {
            *next
                .connectors
                .get_mut(&id)
                .ok_or("reservation_connector_unknown")? = state;
        }
        if matches!(state, ConnectorState::Faulted | ConnectorState::Unavailable) {
            next.reservations
                .retain(|_, r| id != 0 && r.connector_id != id);
            // Preserve every Any reservation whose unbound capacity still exists.
            // Exact reservations on healthy connectors are not spare Any capacity.
            let mut remaining = free_capacity(&next);
            next.reservations.retain(|_, reservation| {
                if reservation.connector_id != 0 {
                    return true;
                }
                if remaining == 0 {
                    return false;
                }
                remaining -= 1;
                true
            });
        }
        if state == ConnectorState::Occupied && !capacity(&next) {
            return Err("reservation_any_capacity_reserved");
        }
        self.commit(next)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        &mut self,
        connector: u32,
        token: &str,
        parent: Option<&str>,
        authorized: bool,
        now: OffsetDateTime,
        expected: Option<i32>,
    ) -> Result<Option<i32>, &'static str> {
        self.expire(now)?;
        if !authorized {
            return Err("reservation_local_authorization_denied");
        }
        if self.state.station_state != ConnectorState::Available
            || self.state.connectors.get(&connector) != Some(&ConnectorState::Available)
        {
            return Err("reservation_connector_not_available");
        }
        let identifies = |r: &ReserveRequest| {
            default_caseless_match_str(token, &r.id_tag)
                || r.parent_id_tag
                    .as_deref()
                    .zip(parent)
                    .is_some_and(|(left, right)| default_caseless_match_str(left, right))
        };
        let exact = self
            .state
            .reservations
            .values()
            .find(|r| r.connector_id == connector);
        if exact.is_some_and(|r| !identifies(r)) {
            return Err("reservation_identity_mismatch");
        }
        let matched = exact.or_else(|| {
            self.state
                .reservations
                .values()
                .find(|r| r.connector_id == 0 && identifies(r))
        });
        let id = matched.map(|r| r.reservation_id);
        if expected.is_some() && expected != id {
            return Err("reservation_start_id_mismatch");
        }
        let mut next = self.state.clone();
        if let Some(id) = id {
            next.reservations.remove(&id);
        }
        next.connectors.insert(connector, ConnectorState::Occupied);
        if !capacity(&next) {
            return Err("reservation_any_capacity_reserved");
        }
        self.commit(next)?;
        Ok(id)
    }
}

fn capacity(state: &PrivateState) -> bool {
    let any = state
        .reservations
        .values()
        .filter(|r| r.connector_id == 0)
        .count();
    free_capacity(state) >= any
}

fn free_capacity(state: &PrivateState) -> usize {
    state
        .connectors
        .iter()
        .filter(|(id, value)| {
            **value == ConnectorState::Available
                && !state.reservations.values().any(|r| r.connector_id == **id)
        })
        .count()
}
