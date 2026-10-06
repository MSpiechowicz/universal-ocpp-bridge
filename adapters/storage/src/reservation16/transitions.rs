use super::{conflict, read_key, save};
use crate::configuration::unavailable;
use rusqlite::{OptionalExtension, Transaction, params};
use uob_application::{ReservationRecord16, StorageError, reservation_live_16};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, ReservationReconciliation16,
    ReservationResult16, ReservationState16,
};

fn evidence(record: &ReservationRecord16) -> ReservationReconciliation16 {
    let state = if record.ambiguous {
        ReservationState16::Ambiguous
    } else {
        record.state
    };
    let observed_at = record
        .source_observed_at
        .map_or(record.changed_at, |at| at.max(record.changed_at));
    ReservationReconciliation16 {
        revision: record.revision,
        state,
        observed_at,
        source_time: record.source_time,
    }
}
#[allow(clippy::too_many_lines)] // One revision-ordered state machine for every result lifecycle.
pub(crate) fn finish(
    transaction: &Transaction<'_>,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let record: Option<(String, bool)> = transaction
        .query_row(
            "SELECT payload,inflight FROM reservations16 WHERE request_id=?1",
            [incoming.return_route.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, current_inflight)) = record else {
        if incoming.reservation_16.is_some() {
            return Err(conflict("reservation acknowledgement lacks durable owner"));
        }
        return Ok(());
    };
    let mut record = crate::codec_reservation16::decode(&payload)?;
    super::codec_validation::correlate(transaction, incoming)?;
    let station = crate::snapshots::station_key(&record.station)?;
    let previous_state = record.state;
    let accepted = incoming
        .reservation_16
        .as_ref()
        .is_some_and(ReservationResult16::accepted);
    let has_native = incoming
        .reservation_16
        .as_ref()
        .is_some_and(|reply| match reply {
            ReservationResult16::ReserveNow { status, .. } => status.is_some(),
            ReservationResult16::CancelReservation { status, .. } => status.is_some(),
        });
    if let Some(reply) = incoming.reservation_16.as_ref().filter(|_| has_native) {
        let valid = match (reply, &record.candidate) {
            (
                ReservationResult16::ReserveNow {
                    reservation_id,
                    connector_id,
                    status: Some(_),
                    ..
                },
                Some(candidate),
            ) => {
                *reservation_id == record.reservation_id && *connector_id == candidate.connector_id
            }
            (
                ReservationResult16::CancelReservation {
                    reservation_id,
                    status: Some(_),
                    ..
                },
                None,
            ) => *reservation_id == record.reservation_id,
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
    let inflight = match incoming.lifecycle {
        // State revisions, not arbitrary result writes, determine historical validity.
        CommandLifecycle::Admitted => current_inflight,
        CommandLifecycle::Dispatched => {
            record.started = true;
            current_inflight
        }
        CommandLifecycle::TransmissionUncertain { .. } => {
            if record.state == ReservationState16::Pending {
                record.state = ReservationState16::Uncertain;
            }
            false
        }
        CommandLifecycle::Rejected { .. } => {
            if matches!(
                record.state,
                ReservationState16::Pending | ReservationState16::Uncertain
            ) {
                record.state = ReservationState16::Rejected;
            }
            record.unresolved = false;
            false
        }
        CommandLifecycle::ProtocolResponse { .. } => {
            record.unresolved = false;
            if accepted {
                for mut previous in read_key(transaction, &station)? {
                    if previous.reservation_id == record.reservation_id
                        && previous.revision < record.revision
                        && reservation_live_16(previous.state)
                    {
                        previous.state =
                            if record.candidate.is_some() || previous.candidate.is_none() {
                                ReservationState16::Superseded
                            } else {
                                ReservationState16::Cancelled
                            };
                        previous.changed_at = previous.changed_at.max(incoming.recorded_at);
                        previous.unresolved = false;
                        save(transaction, &previous, Some(false))?;
                        sync_result(transaction, &previous)?;
                    }
                }
                if reservation_live_16(record.state) {
                    record.state = match &record.candidate {
                        Some(candidate) if candidate.expiry_date <= incoming.recorded_at => {
                            ReservationState16::Expired
                        }
                        Some(_) => ReservationState16::Active,
                        None => ReservationState16::Cancelled,
                    };
                }
            } else if reservation_live_16(record.state) {
                record.state = ReservationState16::Rejected;
            }
            false
        }
    };
    if record.state != previous_state {
        record.changed_at = record.changed_at.max(incoming.recorded_at);
    }
    save(transaction, &record, Some(inflight))?;
    let reconciliation = evidence(&record);
    if let Some(reply) = &mut incoming.reservation_16 {
        *reply.reconciliation_mut() = reconciliation;
    } else {
        incoming.reservation_16 = Some(match record.candidate.as_ref() {
            Some(c) => ReservationResult16::ReserveNow {
                reservation_id: record.reservation_id,
                connector_id: c.connector_id,
                status: None,
                reconciliation,
            },
            None => ReservationResult16::CancelReservation {
                reservation_id: record.reservation_id,
                status: None,
                reconciliation,
            },
        });
    }
    incoming.schema_version = ContractVersion::V1_RESERVATION_16;
    Ok(())
}

pub(super) fn sync_result(
    transaction: &Transaction<'_>,
    record: &ReservationRecord16,
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
        if let Some(reply) = &mut result.reservation_16 {
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
