use super::{conflict, read_key, save, transitions::sync_result};
use rusqlite::Transaction;
use uob_application::{
    MAX_RESERVATION_REVISIONS_16, ReservationObservation16, ReservationObservationKind16,
    ReservationRecord16, StorageError, reservation_live_16, reservation_matches_16,
};
use uob_contracts::{ReservationState16, UtcTimestamp};

#[allow(clippy::too_many_lines)] // Matching must weigh live and historical revisions together.
pub(crate) fn observe(
    transaction: &Transaction<'_>,
    observation: &ReservationObservation16,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&observation.station)?;
    let mut values = read_key(transaction, &station)?;
    // A station clock never extends the reservation's current trusted expiry.
    for record in &mut values {
        if reservation_live_16(record.state)
            && record
                .candidate
                .as_ref()
                .is_some_and(|c| c.expiry_date <= observation.observed_at)
        {
            record.state = ReservationState16::Expired;
            record.changed_at = record.changed_at.max(observation.observed_at);
            record.unresolved = false;
            save(transaction, record, None)?;
            sync_result(transaction, record)?;
        }
    }
    match &observation.kind {
        ReservationObservationKind16::Start {
            reservation_id,
            connector_id,
            token_key,
            group_key,
            source_time,
        } => {
            let mut affected = [false; MAX_RESERVATION_REVISIONS_16];
            let mut tuple_count = 0;
            let mut live_count = 0;
            let mut live_index = None;
            let mut historical_count = 0;
            let mut historical_index = None;
            let mut older_closed_before_source = true;
            for (index, record) in values.iter().enumerate() {
                if record.reservation_id != *reservation_id
                    || !record.started
                    || record.state == ReservationState16::Rejected
                    || !record.candidate.as_ref().is_some_and(|candidate| {
                        reservation_matches_16(
                            candidate,
                            *connector_id,
                            token_key,
                            group_key.as_ref(),
                        )
                    })
                {
                    continue;
                }
                tuple_count += 1;
                if reservation_live_16(record.state) {
                    affected[index] = true;
                    live_count += 1;
                    live_index = Some(index);
                } else {
                    older_closed_before_source &= record.changed_at <= *source_time;
                    if historical_interval(record, *source_time) {
                        affected[index] = true;
                        historical_count += 1;
                        historical_index = Some(index);
                    }
                }
            }
            let history_floor = values.last().and_then(|record| record.history_floor);
            let outside_gap = history_floor.is_none_or(|floor| *source_time > floor);
            let selected = if live_count == 1 {
                let index = live_index.expect("counted live reservation");
                let record = &values[index];
                // A unique never-reused tuple matches at trusted receipt even with
                // a slow station clock. Reuse and discarded history need chronology.
                if (tuple_count == 1 && history_floor.is_none())
                    || (*source_time <= observation.observed_at
                        && record.admitted_at <= *source_time
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
                && *source_time <= observation.observed_at
            {
                historical_index
            } else {
                None
            };
            if let Some(index) = selected {
                let record = &mut values[index];
                if record.state != ReservationState16::Consumed {
                    // A late historical fact must not widen a settled ownership interval.
                    if reservation_live_16(record.state) {
                        record.changed_at = record.changed_at.max(observation.observed_at);
                    }
                    record.state = ReservationState16::Consumed;
                    record.unresolved = false;
                    record.ambiguous = false;
                    record.source_time = Some(*source_time);
                    record.source_observed_at = Some(observation.observed_at);
                    save(transaction, record, None)?;
                    sync_result(transaction, record)?;
                }
            } else {
                for (index, record) in values.iter_mut().enumerate() {
                    if affected[index] {
                        record.ambiguous = true;
                        record.source_time = Some(*source_time);
                        record.source_observed_at = Some(observation.observed_at);
                        // Attribution evidence is not a physical ownership transition.
                        save(transaction, record, None)?;
                        sync_result(transaction, record)?;
                    }
                }
            }
        }
        ReservationObservationKind16::Status {
            connector_id,
            state,
            source_time,
            any_eligible,
        } => {
            if !matches!(
                state,
                ReservationState16::Faulted | ReservationState16::Unavailable
            ) {
                return Err(conflict("invalid reservation status transition"));
            }
            for record in &mut values {
                let applies = record.candidate.as_ref().is_some_and(|c| {
                    c.connector_id == *connector_id || (c.connector_id == 0 && !any_eligible)
                });
                let fresh = source_time
                    .is_none_or(|time| time >= record.admitted_at && time >= record.changed_at);
                if applies && fresh && reservation_live_16(record.state) {
                    record.state = *state;
                    record.changed_at = record.changed_at.max(observation.observed_at);
                    record.source_time = *source_time;
                    record.source_observed_at = Some(observation.observed_at);
                    record.unresolved = false;
                    save(transaction, record, None)?;
                    sync_result(transaction, record)?;
                }
            }
        }
        ReservationObservationKind16::Expiry => {}
    }
    Ok(())
}

fn historical_interval(record: &ReservationRecord16, source_time: UtcTimestamp) -> bool {
    record.admitted_at <= source_time
        && source_time < record.changed_at
        && record
            .candidate
            .as_ref()
            .is_some_and(|candidate| source_time < candidate.expiry_date)
}
