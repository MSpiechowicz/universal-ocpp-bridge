use super::{conflict, read_key, save};
use crate::configuration::unavailable;
use rusqlite::{OptionalExtension, Transaction, params};
use uob_application::{FirmwareJobRecord201, StorageError};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, FirmwareJob201, FirmwareJobState201,
    FirmwareResult201,
};

pub(super) fn evidence(record: &FirmwareJobRecord201) -> FirmwareJob201 {
    FirmwareJob201 {
        revision: record.revision,
        state: record.state,
        deadline: record.deadline,
        observed_at: record.changed_at,
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        rejected_transitions: record.rejected_transitions,
    }
}

const fn unanswered(state: FirmwareJobState201) -> bool {
    matches!(
        state,
        FirmwareJobState201::Pending | FirmwareJobState201::Uncertain
    )
}

/// Applies a command lifecycle to its durable job and fills the public job evidence.
pub(crate) fn finish(
    transaction: &Transaction<'_>,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let record: Option<(String, bool)> = transaction
        .query_row(
            "SELECT payload,inflight FROM firmware201_jobs WHERE request_id=?1",
            [incoming.return_route.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, current_inflight)) = record else {
        if incoming.firmware_201.is_some() {
            return Err(conflict("firmware reply lacks durable job owner"));
        }
        return Ok(());
    };
    let mut record = crate::codec_firmware201::decode(&payload)?;
    super::validation::correlate(transaction, incoming)?;
    let previous_state = record.state;
    let reply = incoming
        .firmware_201
        .as_ref()
        .and_then(|result| result.reply.clone());
    if matches!(
        incoming.lifecycle,
        CommandLifecycle::ProtocolResponse { .. }
    ) != reply.is_some()
    {
        return Err(conflict("firmware protocol response lacks native reply"));
    }
    let inflight = match &incoming.lifecycle {
        CommandLifecycle::Admitted => current_inflight,
        CommandLifecycle::Dispatched => {
            record.started = true;
            current_inflight
        }
        CommandLifecycle::TransmissionUncertain { .. } => {
            if record.state == FirmwareJobState201::Pending {
                record.state = FirmwareJobState201::Uncertain;
            }
            false
        }
        CommandLifecycle::Rejected { .. } => {
            if record.state == FirmwareJobState201::Pending {
                record.state = FirmwareJobState201::NotSent;
            }
            false
        }
        CommandLifecycle::ProtocolResponse { .. } => {
            let reply = reply.expect("checked native reply");
            if reply.accepted() {
                retire_older(
                    transaction,
                    &record,
                    reply.cancelled_previous(),
                    incoming.recorded_at,
                )?;
                if unanswered(record.state) {
                    record.state = FirmwareJobState201::Accepted;
                }
            } else if unanswered(record.state) {
                record.state = FirmwareJobState201::Rejected;
            }
            false
        }
    };
    if record.state != previous_state {
        record.changed_at = record.changed_at.max(incoming.recorded_at);
    }
    save(transaction, &record, Some(inflight))?;
    let job = evidence(&record);
    if let Some(result) = &mut incoming.firmware_201 {
        result.job = job;
    } else {
        incoming.firmware_201 = Some(FirmwareResult201 {
            request_id: record.native_request_id,
            secure: record.secure,
            artifact: None,
            reply: None,
            job,
        });
    }
    incoming.schema_version = ContractVersion::V1_FIRMWARE_201;
    Ok(())
}

/// An accepted request ends every older unresolved job of the station: the station either
/// cancelled it (`AcceptedCanceled`, L01.FR.24) or no longer had it in progress.
fn retire_older(
    transaction: &Transaction<'_>,
    record: &FirmwareJobRecord201,
    cancelled: bool,
    at: uob_contracts::UtcTimestamp,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&record.station)?;
    for mut older in read_key(transaction, &station)? {
        if older.revision >= record.revision || older.state.resolved() {
            continue;
        }
        older.state = if cancelled {
            FirmwareJobState201::Cancelled
        } else {
            FirmwareJobState201::Superseded
        };
        older.changed_at = older.changed_at.max(at);
        save(transaction, &older, Some(false))?;
        sync_result(transaction, &older)?;
    }
    Ok(())
}

/// Refreshes the retained public result after an independent state change.
pub(super) fn sync_result(
    transaction: &rusqlite::Connection,
    record: &FirmwareJobRecord201,
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
        if let Some(evidence_slot) = &mut result.firmware_201 {
            evidence_slot.job = evidence(record);
            transaction
                .execute(
                    "UPDATE command_results SET payload=?2 WHERE request_id=?1",
                    params![
                        record.request_id.as_str(),
                        serde_json::to_string(&result)
                            .map_err(|_| conflict("firmware result encoding failed"))?
                    ],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}
