use super::capacity::{
    Types, evse_status, exact_reserved, feasible, refusal, targeted, typed, view,
};
use super::{
    ConnectorKey, ConnectorStatus, Model, PrivateState, RESERVATION_LIMIT, Reservation201Policy,
    ReserveNowRequest, ReserveStatus, StatusUpdate, UPDATE_LIMIT, UpdateStatus,
};
use crate::local_authorization201::IdToken;
use caseless::default_caseless_match_str;
use serde_json::Value;
use std::sync::LazyLock;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

static CONNECTOR_TYPES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReserveNowRequest.json"
    ))
    .expect("pinned schema");
    schema["definitions"]["ConnectorEnumType"]["enum"]
        .as_array()
        .expect("pinned connector enumeration")
        .iter()
        .map(|value| value.as_str().expect("pinned connector type").to_owned())
        .collect()
});

pub(super) fn types(
    evses: &[ConnectorKey],
    policy: &Reservation201Policy,
) -> Result<Types, &'static str> {
    let mut result = Types::new();
    for definition in &policy.connector_types {
        let key = (definition.evse, definition.connector);
        if !evses.contains(&key)
            || !CONNECTOR_TYPES.contains(&definition.connector_type)
            || result
                .insert(key, definition.connector_type.clone())
                .is_some()
        {
            return Err("reservation_connector_type_invalid");
        }
    }
    Ok(result)
}

pub(super) fn validate(
    state: &PrivateState,
    station: &str,
    evses: &[ConnectorKey],
) -> Result<(), &'static str> {
    let stored = state
        .evses
        .iter()
        .flat_map(|(evse, connectors)| connectors.keys().map(move |connector| (*evse, *connector)))
        .collect::<Vec<_>>();
    let mut expected = evses.to_vec();
    expected.sort_unstable();
    if state.format_version != 1
        || state.station != station
        || state.protocol != "ocpp2.0.1"
        || stored != expected
        || state.reservations.len() > RESERVATION_LIMIT
        || state.updates.len() > UPDATE_LIMIT
        || state.reservations.iter().any(|(id, r)| {
            *id != r.id
                || !valid(r)
                || r.evse_id.is_some_and(|evse| {
                    u16::try_from(evse)
                        .ok()
                        .is_none_or(|evse| !state.evses.contains_key(&evse))
                })
        })
    {
        return Err("reservation_state_invalid");
    }
    Ok(())
}

pub(super) fn expiry(request: &ReserveNowRequest) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(&request.expiry_date_time, &Rfc3339).ok()
}

pub(super) fn valid(request: &ReserveNowRequest) -> bool {
    let token = |token: &IdToken| token.id_token.chars().count() <= 36;
    expiry(request).is_some()
        && token(&request.id_token)
        && request.group_id_token.as_ref().is_none_or(token)
        && request
            .connector_type
            .as_ref()
            .is_none_or(|kind| CONNECTOR_TYPES.contains(kind))
}

fn same(left: &IdToken, right: &IdToken) -> bool {
    left.token_type == right.token_type
        && default_caseless_match_str(&left.id_token, &right.id_token)
}

/// H03.FR.01–06: the reservation's idToken, or its groupIdToken equal to the presented group.
fn identifies(reservation: &ReserveNowRequest, token: &IdToken, group: Option<&IdToken>) -> bool {
    same(&reservation.id_token, token)
        || reservation
            .group_id_token
            .as_ref()
            .zip(group)
            .is_some_and(|(left, right)| same(left, right))
}

impl Model {
    fn commit(&mut self, mut next: PrivateState, quiet: Option<u16>) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("reservation_state_unavailable");
        }
        if next.updates.len() > UPDATE_LIMIT {
            return Err("reservation_update_capacity");
        }
        next.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or("reservation_revision_capacity")?;
        let before = view(&self.state, &self.types);
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
        // The station's own explicit report for the touched EVSE is already on the wire.
        for (key, reported) in view(&self.state, &self.types) {
            if before.get(&key) != Some(&reported) && Some(key.0) != quiet {
                self.statuses.retain(|(queued, _)| *queued != key);
                self.statuses.push((key, reported));
            }
        }
        Ok(())
    }

    pub(super) fn acknowledge(&mut self, update: StatusUpdate) -> Result<(), &'static str> {
        let Some(index) = self
            .state
            .updates
            .iter()
            .position(|queued| *queued == update)
        else {
            return Ok(());
        };
        let mut next = self.state.clone();
        next.updates.remove(index);
        self.commit(next, None)
    }

    pub(super) fn expire(&mut self, now: OffsetDateTime) -> Result<(), &'static str> {
        if self.unavailable {
            return Err("reservation_state_unavailable");
        }
        let expired: Vec<i32> = self
            .state
            .reservations
            .values()
            .filter(|r| expiry(r).is_some_and(|at| at <= now))
            .map(|r| r.id)
            .collect();
        if expired.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        for id in expired {
            next.reservations.remove(&id);
            next.updates.push(StatusUpdate {
                reservation_id: id,
                reservation_update_status: UpdateStatus::Expired,
            });
        }
        self.commit(next, None)
    }

    pub(super) fn reserve(
        &mut self,
        request: ReserveNowRequest,
        now: OffsetDateTime,
    ) -> Result<ReserveStatus, &'static str> {
        if !valid(&request) {
            return Err("reservation_request_invalid");
        }
        self.expire(now)?;
        if !self.enabled || expiry(&request).is_none_or(|at| at <= now) {
            return Ok(ReserveStatus::Rejected);
        }
        let mut next = self.state.clone();
        // A refused replacement discards `next`, so the previous reservation stays intact.
        next.reservations.remove(&request.id);
        if next.reservations.len() >= RESERVATION_LIMIT {
            return Ok(ReserveStatus::Rejected);
        }
        let wanted = request.connector_type.clone();
        let status = match request.evse_id {
            Some(evse) => {
                let Some(evse) = u16::try_from(evse)
                    .ok()
                    .filter(|evse| next.evses.contains_key(evse))
                else {
                    return Ok(ReserveStatus::Rejected);
                };
                let connectors = targeted(&next, &self.types, evse, wanted.as_deref());
                if connectors.is_empty() {
                    return Ok(ReserveStatus::Rejected);
                }
                if evse_status(&next.evses[&evse]) == ConnectorStatus::Occupied
                    || exact_reserved(&next, evse)
                {
                    return Ok(ReserveStatus::Occupied);
                }
                if !connectors.contains(&ConnectorStatus::Available) {
                    return Ok(refusal(&connectors));
                }
                next.reservations.insert(request.id, request);
                if feasible(&next, &self.types, None) {
                    ReserveStatus::Accepted
                } else {
                    ReserveStatus::Occupied
                }
            }
            None if !self.non_evse_specific => return Ok(ReserveStatus::Rejected),
            None => {
                let connectors: Vec<_> = next
                    .evses
                    .keys()
                    .flat_map(|evse| targeted(&next, &self.types, *evse, wanted.as_deref()))
                    .collect();
                if connectors.is_empty() {
                    return Ok(ReserveStatus::Rejected);
                }
                next.reservations.insert(request.id, request);
                if feasible(&next, &self.types, None) {
                    ReserveStatus::Accepted
                } else if connectors.contains(&ConnectorStatus::Available) {
                    ReserveStatus::Occupied
                } else {
                    refusal(&connectors)
                }
            }
        };
        if status == ReserveStatus::Accepted {
            self.commit(next, None)?;
        }
        Ok(status)
    }

    pub(super) fn cancel(&mut self, id: i32, now: OffsetDateTime) -> Result<bool, &'static str> {
        self.expire(now)?;
        if !self.state.reservations.contains_key(&id) {
            return Ok(false);
        }
        let mut next = self.state.clone();
        next.reservations.remove(&id);
        self.commit(next, None)?;
        Ok(true)
    }

    pub(super) fn set_connector(
        &mut self,
        evse: u16,
        connector: u16,
        status: ConnectorStatus,
    ) -> Result<(), &'static str> {
        let mut next = self.state.clone();
        let slot = next
            .evses
            .get_mut(&evse)
            .and_then(|connectors| connectors.get_mut(&connector))
            .ok_or("reservation_connector_unknown")?;
        if *slot == status {
            return Ok(());
        }
        *slot = status;
        let before = evse_status(&self.state.evses[&evse]);
        let after = evse_status(&next.evses[&evse]);
        let mut removed = Vec::new();
        if before != after
            && matches!(
                after,
                ConnectorStatus::Faulted | ConnectorStatus::Unavailable
            )
        {
            // H01.FR.16/17: the station cancels and reports Removed.
            removed.extend(
                next.reservations
                    .values()
                    .filter(|r| r.evse_id == Some(i32::from(evse)))
                    .map(|r| r.id),
            );
        }
        if after == ConnectorStatus::Occupied && before != after && exact_reserved(&next, evse) {
            return Err("reservation_evse_reserved");
        }
        for id in &removed {
            next.reservations.remove(id);
        }
        if !feasible(&next, &self.types, None) {
            if after == ConnectorStatus::Occupied {
                return Err("reservation_capacity_reserved");
            }
            removed.extend(shed_unbound(&mut next, &self.types));
        }
        next.updates
            .extend(removed.into_iter().map(|id| StatusUpdate {
                reservation_id: id,
                reservation_update_status: UpdateStatus::Removed,
            }));
        self.commit(next, Some(evse))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        &mut self,
        evse: u16,
        connector: u16,
        identity: Option<(&IdToken, Option<&IdToken>)>,
        authorized: bool,
        now: OffsetDateTime,
        expected: Option<i32>,
    ) -> Result<Option<i32>, &'static str> {
        self.expire(now)?;
        if identity.is_some() && !authorized {
            return Err("reservation_local_authorization_denied");
        }
        let connectors = self
            .state
            .evses
            .get(&evse)
            .ok_or("reservation_connector_unknown")?;
        if evse_status(connectors) != ConnectorStatus::Available
            || connectors.get(&connector) != Some(&ConnectorStatus::Available)
        {
            return Err("reservation_connector_not_available");
        }
        let fits = |r: &ReserveNowRequest| {
            identity.is_some_and(|(token, group)| identifies(r, token, group))
                && typed(&self.types, (evse, connector), r.connector_type.as_deref())
        };
        let exact = self
            .state
            .reservations
            .values()
            .find(|r| r.evse_id == Some(i32::from(evse)));
        if exact.is_some_and(|r| !fits(r)) {
            return Err(if identity.is_some() {
                "reservation_identity_mismatch"
            } else {
                "reservation_identity_required"
            });
        }
        let matched = exact
            .or_else(|| {
                self.state
                    .reservations
                    .values()
                    .find(|r| r.evse_id.is_none() && fits(r))
            })
            .map(|r| r.id);
        if expected.is_some() && expected != matched {
            return Err("reservation_start_id_mismatch");
        }
        let mut next = self.state.clone();
        if let Some(id) = matched {
            next.reservations.remove(&id);
        }
        next.evses
            .get_mut(&evse)
            .and_then(|connectors| connectors.get_mut(&connector))
            .map(|slot| *slot = ConnectorStatus::Occupied)
            .ok_or("reservation_connector_unknown")?;
        if !feasible(&next, &self.types, None) {
            return Err("reservation_capacity_reserved");
        }
        self.commit(next, Some(evse))?;
        Ok(matched)
    }
}

/// Keep each unspecified reservation whose capacity still exists, in reservation ID order.
fn shed_unbound(next: &mut PrivateState, types: &Types) -> Vec<i32> {
    let unbound: Vec<i32> = next
        .reservations
        .values()
        .filter(|r| r.evse_id.is_none())
        .map(|r| r.id)
        .collect();
    let mut parked: Vec<ReserveNowRequest> = unbound
        .iter()
        .filter_map(|id| next.reservations.remove(id))
        .collect();
    let mut removed = Vec::new();
    for reservation in parked.drain(..) {
        let id = reservation.id;
        next.reservations.insert(id, reservation);
        if !feasible(next, types, None) {
            next.reservations.remove(&id);
            removed.push(id);
        }
    }
    removed
}
