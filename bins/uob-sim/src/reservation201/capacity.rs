//! Physical capacity and reported connector views; no decision depends on a reply text.
use super::{ConnectorKey, ConnectorStatus, PrivateState, ReserveNowRequest, ReserveStatus};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Types = BTreeMap<ConnectorKey, String>;

/// One session per EVSE: any Occupied connector occupies it, any Available one keeps it usable.
pub(super) fn evse_status(connectors: &BTreeMap<u16, ConnectorStatus>) -> ConnectorStatus {
    let any = |wanted| connectors.values().any(|status| *status == wanted);
    if any(ConnectorStatus::Occupied) {
        ConnectorStatus::Occupied
    } else if any(ConnectorStatus::Available) {
        ConnectorStatus::Available
    } else if connectors
        .values()
        .all(|status| *status == ConnectorStatus::Faulted)
    {
        ConnectorStatus::Faulted
    } else {
        ConnectorStatus::Unavailable
    }
}

pub(super) fn typed(types: &Types, key: ConnectorKey, wanted: Option<&str>) -> bool {
    wanted.is_none_or(|wanted| types.get(&key).is_some_and(|actual| actual == wanted))
}

/// Actual states of the connectors a request targets on one EVSE (all, or its connectorType).
pub(super) fn targeted(
    state: &PrivateState,
    types: &Types,
    evse: u16,
    wanted: Option<&str>,
) -> Vec<ConnectorStatus> {
    state.evses.get(&evse).map_or_else(Vec::new, |connectors| {
        connectors
            .iter()
            .filter(|(connector, _)| typed(types, (evse, **connector), wanted))
            .map(|(_, status)| *status)
            .collect()
    })
}

pub(super) fn exact_reserved(state: &PrivateState, evse: u16) -> bool {
    state
        .reservations
        .values()
        .any(|reservation| reservation.evse_id == Some(i32::from(evse)))
}

/// A free EVSE an unspecified reservation could still be served on (FR.07/09).
fn usable(
    state: &PrivateState,
    types: &Types,
    evse: u16,
    reservation: &ReserveNowRequest,
    excluded: Option<u16>,
) -> bool {
    let Some(connectors) = state.evses.get(&evse) else {
        return false;
    };
    Some(evse) != excluded
        && evse_status(connectors) == ConnectorStatus::Available
        && !exact_reserved(state, evse)
        && connectors.iter().any(|(connector, status)| {
            *status == ConnectorStatus::Available
                && typed(
                    types,
                    (evse, *connector),
                    reservation.connector_type.as_deref(),
                )
        })
}

/// Every reservation without evseId keeps its own distinct usable EVSE (bipartite matching).
pub(super) fn feasible(state: &PrivateState, types: &Types, excluded: Option<u16>) -> bool {
    let unbound: Vec<&ReserveNowRequest> = state
        .reservations
        .values()
        .filter(|reservation| reservation.evse_id.is_none())
        .collect();
    let mut owners = BTreeMap::new();
    (0..unbound.len()).all(|index| {
        augment(
            &Matching {
                state,
                types,
                unbound: &unbound,
                excluded,
            },
            index,
            &mut owners,
            &mut BTreeSet::new(),
        )
    })
}

struct Matching<'a> {
    state: &'a PrivateState,
    types: &'a Types,
    unbound: &'a [&'a ReserveNowRequest],
    excluded: Option<u16>,
}

fn augment(
    matching: &Matching<'_>,
    index: usize,
    owners: &mut BTreeMap<u16, usize>,
    seen: &mut BTreeSet<u16>,
) -> bool {
    for evse in matching.state.evses.keys() {
        if !usable(
            matching.state,
            matching.types,
            *evse,
            matching.unbound[index],
            matching.excluded,
        ) || !seen.insert(*evse)
        {
            continue;
        }
        let previous = owners.get(evse).copied();
        if previous.is_none_or(|other| augment(matching, other, owners, seen)) {
            owners.insert(*evse, index);
            return true;
        }
    }
    false
}

/// FR.23 and errata FR.20/24: all connectors of an exactly reserved EVSE, or of a free EVSE the
/// unspecified reservations cannot spare, report Reserved.
pub(super) fn view(state: &PrivateState, types: &Types) -> BTreeMap<ConnectorKey, &'static str> {
    let unbound = state
        .reservations
        .values()
        .any(|reservation| reservation.evse_id.is_none());
    let satisfied = unbound && feasible(state, types, None);
    let mut result = BTreeMap::new();
    for (evse, connectors) in &state.evses {
        let reserved = exact_reserved(state, *evse)
            || (satisfied
                && evse_status(connectors) == ConnectorStatus::Available
                && !feasible(state, types, Some(*evse)));
        for (connector, status) in connectors {
            let reported = match status {
                ConnectorStatus::Available if reserved => "Reserved",
                ConnectorStatus::Available => "Available",
                ConnectorStatus::Occupied => "Occupied",
                ConnectorStatus::Unavailable => "Unavailable",
                ConnectorStatus::Faulted => "Faulted",
            };
            result.insert((*evse, *connector), reported);
        }
    }
    result
}

/// FR.11 (errata), FR.12 and FR.14 from the actual targeted connectors.
pub(super) fn refusal(targeted: &[ConnectorStatus]) -> ReserveStatus {
    if !targeted.is_empty()
        && targeted
            .iter()
            .all(|status| *status == ConnectorStatus::Faulted)
    {
        ReserveStatus::Faulted
    } else if !targeted.is_empty()
        && targeted.iter().all(|status| {
            matches!(
                status,
                ConnectorStatus::Faulted | ConnectorStatus::Unavailable
            )
        })
    {
        ReserveStatus::Unavailable
    } else {
        ReserveStatus::Occupied
    }
}
