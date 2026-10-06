use super::{conflict, read_key, save};
use crate::configuration::unavailable;
use rusqlite::{OptionalExtension, Transaction, params};
use uob_application::{ReservationRecord201, StorageError, reservation_live_201};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, ReservationReconciliation201,
    ReservationResult201, ReservationState201,
};

pub(super) fn evidence(record: &ReservationRecord201) -> ReservationReconciliation201 {
    let state = if record.ambiguous {
        ReservationState201::Ambiguous
    } else {
        record.state
    };
    let observed_at = record
        .source_observed_at
        .map_or(record.changed_at, |at| at.max(record.changed_at));
    ReservationReconciliation201 {
        revision: record.revision,
        state,
        observed_at,
        source_time: record.source_time,
    }
}
/// One revision-ordered state machine for every result lifecycle of an owned 2.0.1 mutation.
pub(crate) fn finish(
    transaction: &Transaction<'_>,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let record: Option<(String, bool)> = transaction
        .query_row(
            "SELECT payload,inflight FROM reservations201 WHERE request_id=?1",
            [incoming.return_route.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, current_inflight)) = record else {
        if incoming.reservation_201.is_some() {
            return Err(conflict("reservation acknowledgement lacks durable owner"));
        }
        return Ok(());
    };
    let mut record = crate::codec_reservation201::decode(&payload)?;
    super::codec_validation::correlate(transaction, incoming)?;
    let station = crate::snapshots::station_key(&record.station)?;
    let previous_state = record.state;
    let accepted = validate_reply(&record, incoming)?;
    let inflight = match incoming.lifecycle {
        // State revisions, not arbitrary result writes, determine historical validity.
        CommandLifecycle::Admitted => current_inflight,
        CommandLifecycle::Dispatched => {
            record.started = true;
            current_inflight
        }
        CommandLifecycle::TransmissionUncertain { .. } => {
            if record.state == ReservationState201::Pending {
                record.state = ReservationState201::Uncertain;
            }
            false
        }
        CommandLifecycle::Rejected { .. } => {
            if matches!(
                record.state,
                ReservationState201::Pending | ReservationState201::Uncertain
            ) {
                record.state = ReservationState201::Rejected;
            }
            record.unresolved = false;
            false
        }
        CommandLifecycle::ProtocolResponse { .. } => {
            record.unresolved = false;
            if accepted {
                acknowledge(transaction, &station, &mut record, incoming)?;
            } else if reservation_live_201(record.state) {
                // H01.FR.02: only an accepted replacement displaces the previous owner.
                record.state = ReservationState201::Rejected;
            }
            false
        }
    };
    if record.state != previous_state {
        record.changed_at = record.changed_at.max(incoming.recorded_at);
    }
    save(transaction, &record, Some(inflight))?;
    let reconciliation = evidence(&record);
    if let Some(reply) = &mut incoming.reservation_201 {
        *reply.reconciliation_mut() = reconciliation;
    } else {
        incoming.reservation_201 = Some(match record.candidate.as_ref() {
            Some(c) => ReservationResult201::ReserveNow {
                reservation_id: record.reservation_id,
                evse_id: c.evse_id,
                status: None,
                reconciliation,
            },
            None => ReservationResult201::CancelReservation {
                reservation_id: record.reservation_id,
                status: None,
                reconciliation,
            },
        });
    }
    incoming.schema_version = ContractVersion::V1_RESERVATION_201;
    Ok(())
}

/// The native reply must still describe the owned action and agree with its lifecycle.
fn validate_reply(
    record: &ReservationRecord201,
    incoming: &CommandResult,
) -> Result<bool, StorageError> {
    let accepted = incoming
        .reservation_201
        .as_ref()
        .is_some_and(ReservationResult201::accepted);
    let has_native = incoming
        .reservation_201
        .as_ref()
        .is_some_and(ReservationResult201::has_native_status);
    if let Some(reply) = incoming.reservation_201.as_ref().filter(|_| has_native) {
        let valid = match (reply, &record.candidate) {
            (
                ReservationResult201::ReserveNow {
                    reservation_id,
                    evse_id,
                    ..
                },
                Some(candidate),
            ) => *reservation_id == record.reservation_id && *evse_id == candidate.evse_id,
            (ReservationResult201::CancelReservation { reservation_id, .. }, None) => {
                *reservation_id == record.reservation_id
            }
            _ => false,
        };
        if !valid
            || !matches!(incoming.lifecycle, CommandLifecycle::ProtocolResponse { accepted: a, .. } if a == accepted)
        {
            return Err(conflict(
                "reservation acknowledgement changed reserved action",
            ));
        }
    }
    if matches!(
        incoming.lifecycle,
        CommandLifecycle::ProtocolResponse { .. }
    ) && !has_native
    {
        return Err(conflict(
            "reservation protocol response lacks native status",
        ));
    }
    Ok(accepted)
}

fn acknowledge(
    transaction: &Transaction<'_>,
    station: &str,
    record: &mut ReservationRecord201,
    incoming: &CommandResult,
) -> Result<(), StorageError> {
    for mut previous in read_key(transaction, station)? {
        if previous.reservation_id == record.reservation_id
            && previous.revision < record.revision
            && reservation_live_201(previous.state)
        {
            previous.state = if record.candidate.is_some() || previous.candidate.is_none() {
                ReservationState201::Superseded
            } else {
                ReservationState201::Cancelled
            };
            previous.changed_at = previous.changed_at.max(incoming.recorded_at);
            previous.unresolved = false;
            save(transaction, &previous, Some(false))?;
            sync_result(transaction, &previous)?;
        }
    }
    if reservation_live_201(record.state) {
        record.state = match &record.candidate {
            Some(candidate) if candidate.expiry_date_time <= incoming.recorded_at => {
                ReservationState201::Expired
            }
            Some(_) => ReservationState201::Active,
            None => ReservationState201::Cancelled,
        };
    }
    Ok(())
}

pub(super) fn sync_result(
    transaction: &Transaction<'_>,
    record: &ReservationRecord201,
) -> Result<(), StorageError> {
    let payload: Option<String> = transaction
        .query_row(
            "SELECT payload FROM command_results WHERE request_id=?1",
            [record.request_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    if let Some(payload) = payload {
        let mut result = crate::codec::decode_result(&payload)?;
        if let Some(reply) = &mut result.reservation_201 {
            *reply.reconciliation_mut() = evidence(record);
            transaction
                .execute(
                    "UPDATE command_results SET payload=?2 WHERE request_id=?1",
                    params![
                        record.request_id.as_str(),
                        serde_json::to_string(&result)
                            .map_err(|_| conflict("reservation result encoding failed"))?
                    ],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}
