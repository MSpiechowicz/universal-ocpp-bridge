use super::{read_key, save, transitions::sync_result};
use rusqlite::Transaction;
use uob_application::{
    MAX_RESERVATION_REVISIONS_201, ReservationKey201, ReservationObservation201,
    ReservationObservationKind201, ReservationRecord201, ReservationUpdateStatus201, StorageError,
    reservation_live_201, reservation_matches_201,
};
use uob_contracts::{ReservationState201, UtcTimestamp};

pub(crate) fn observe(
    transaction: &Transaction<'_>,
    observation: &ReservationObservation201,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&observation.station)?;
    let mut values = read_key(transaction, &station)?;
    // A station clock never extends the reservation's current trusted expiry.
    for record in &mut values {
        if reservation_live_201(record.state)
            && record
                .candidate
                .as_ref()
                .is_some_and(|c| c.expiry_date_time <= observation.observed_at)
        {
            record.state = ReservationState201::Expired;
            record.changed_at = record.changed_at.max(observation.observed_at);
            record.unresolved = false;
            save(transaction, record, None)?;
            sync_result(transaction, record)?;
        }
    }
    match &observation.kind {
        ReservationObservationKind201::Transaction {
            reservation_id,
            evse_id,
            token_key,
            group_key,
            source_time,
        } => consume(
            transaction,
            &mut values,
            observation.observed_at,
            &Attribution {
                reservation_id: *reservation_id,
                evse_id: *evse_id,
                token_key: token_key.as_ref(),
                group_key: group_key.as_ref(),
                source_time: *source_time,
            },
        ),
        ReservationObservationKind201::StatusUpdate {
            reservation_id,
            status,
        } => update(
            transaction,
            &mut values,
            observation.observed_at,
            *reservation_id,
            *status,
        ),
        ReservationObservationKind201::Expiry => Ok(()),
    }
}

struct Attribution<'a> {
    reservation_id: i32,
    evse_id: u32,
    token_key: Option<&'a ReservationKey201>,
    group_key: Option<&'a ReservationKey201>,
    source_time: UtcTimestamp,
}

/// Explicit native termination. CSMS cancellation never produces this message (H02 remark),
/// so only sent reservation revisions that are still live can be its subject.
fn update(
    transaction: &Transaction<'_>,
    values: &mut [ReservationRecord201],
    observed_at: UtcTimestamp,
    reservation_id: i32,
    status: ReservationUpdateStatus201,
) -> Result<(), StorageError> {
    let subjects = values
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.reservation_id == reservation_id
                && record.started
                && record.candidate.is_some()
                && reservation_live_201(record.state)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let single = subjects.len() == 1;
    for index in subjects {
        let record = &mut values[index];
        if single {
            record.state = status.state();
            record.changed_at = record.changed_at.max(observed_at);
            record.unresolved = false;
            record.ambiguous = false;
            record.source_time = None;
        } else {
            // An in-flight same-ID replacement makes the station's subject unknowable.
            record.ambiguous = true;
        }
        record.source_observed_at = Some(observed_at);
        save(transaction, record, None)?;
        sync_result(transaction, record)?;
    }
    Ok(())
}

/// The station's reservationId is authoritative only within one reservation-ID ownership
/// interval; reused IDs and discarded history need chronology before attribution.
fn consume(
    transaction: &Transaction<'_>,
    values: &mut [ReservationRecord201],
    observed_at: UtcTimestamp,
    fact: &Attribution<'_>,
) -> Result<(), StorageError> {
    let mut affected = [false; MAX_RESERVATION_REVISIONS_201];
    let mut tuple_count = 0;
    let mut live_count = 0;
    let mut live_index = None;
    let mut historical_count = 0;
    let mut historical_index = None;
    let mut older_closed_before_source = true;
    for (index, record) in values.iter().enumerate() {
        if record.reservation_id != fact.reservation_id
            || !record.started
            || record.state == ReservationState201::Rejected
            || !record.candidate.as_ref().is_some_and(|candidate| {
                reservation_matches_201(candidate, fact.evse_id, fact.token_key, fact.group_key)
            })
        {
            continue;
        }
        tuple_count += 1;
        if reservation_live_201(record.state) {
            affected[index] = true;
            live_count += 1;
            live_index = Some(index);
        } else {
            older_closed_before_source &= record.changed_at <= fact.source_time;
            if historical_interval(record, fact.source_time) {
                affected[index] = true;
                historical_count += 1;
                historical_index = Some(index);
            }
        }
    }
    let history_floor = values.last().and_then(|record| record.history_floor);
    let outside_gap = history_floor.is_none_or(|floor| fact.source_time > floor);
    let selected = if live_count == 1 {
        let index = live_index.expect("counted live reservation");
        let record = &values[index];
        // A unique never-reused tuple matches at trusted receipt even with a slow
        // station clock. Reuse and discarded history need chronology.
        if (tuple_count == 1 && history_floor.is_none())
            || (fact.source_time <= observed_at
                && record.admitted_at <= fact.source_time
                && older_closed_before_source
                && historical_count == 0
                && outside_gap)
        {
            Some(index)
        } else {
            None
        }
    } else if live_count == 0
        && historical_count == 1
        && outside_gap
        && fact.source_time <= observed_at
    {
        historical_index
    } else {
        None
    };
    if let Some(index) = selected {
        let record = &mut values[index];
        if record.state != ReservationState201::Consumed {
            // A late historical fact must not widen a settled ownership interval.
            if reservation_live_201(record.state) {
                record.changed_at = record.changed_at.max(observed_at);
            }
            record.state = ReservationState201::Consumed;
            record.unresolved = false;
            record.ambiguous = false;
            record.source_time = Some(fact.source_time);
            record.source_observed_at = Some(observed_at);
            save(transaction, record, None)?;
            sync_result(transaction, record)?;
        }
    } else {
        for (index, record) in values.iter_mut().enumerate() {
            if affected[index] {
                record.ambiguous = true;
                record.source_time = Some(fact.source_time);
                record.source_observed_at = Some(observed_at);
                // Attribution evidence is not a physical ownership transition.
                save(transaction, record, None)?;
                sync_result(transaction, record)?;
            }
        }
    }
    Ok(())
}

fn historical_interval(record: &ReservationRecord201, source_time: UtcTimestamp) -> bool {
    record.admitted_at <= source_time
        && source_time < record.changed_at
        && record
            .candidate
            .as_ref()
            .is_some_and(|candidate| source_time < candidate.expiry_date_time)
}
