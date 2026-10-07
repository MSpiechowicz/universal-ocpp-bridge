use super::{conflict, read_key, save};
use crate::configuration::unavailable;
use rusqlite::{OptionalExtension, Transaction, params};
use uob_application::{DiagnosticsJobRecord201, StorageError};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, DiagnosticsJob201, DiagnosticsJobState201,
    DiagnosticsReply201, DiagnosticsResult201,
};

pub(super) fn evidence(record: &DiagnosticsJobRecord201) -> DiagnosticsJob201 {
    DiagnosticsJob201 {
        revision: record.revision,
        state: record.state,
        deadline: record.deadline,
        observed_at: record.changed_at,
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        upload: record.upload.clone(),
    }
}

const fn unanswered(state: DiagnosticsJobState201) -> bool {
    matches!(
        state,
        DiagnosticsJobState201::Pending | DiagnosticsJobState201::Uncertain
    )
}

/// Applies a command lifecycle to its durable job and fills the public job evidence.
pub(crate) fn finish(
    transaction: &Transaction<'_>,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let record: Option<(String, bool)> = transaction
        .query_row(
            "SELECT payload,inflight FROM diagnostics201_jobs WHERE request_id=?1",
            [incoming.return_route.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, current_inflight)) = record else {
        if incoming.diagnostics_201.is_some() {
            return Err(conflict("log reply lacks durable job owner"));
        }
        return Ok(());
    };
    let mut record = crate::codec_diagnostics201::decode(&payload)?;
    super::validation::correlate(transaction, incoming)?;
    let previous_state = record.state;
    let reply = incoming
        .diagnostics_201
        .as_ref()
        .and_then(|result| result.reply.clone());
    if matches!(
        incoming.lifecycle,
        CommandLifecycle::ProtocolResponse { .. }
    ) != reply.is_some()
    {
        return Err(conflict("log protocol response lacks native reply"));
    }
    let inflight = match &incoming.lifecycle {
        CommandLifecycle::Admitted => current_inflight,
        CommandLifecycle::Dispatched => {
            record.started = true;
            current_inflight
        }
        CommandLifecycle::TransmissionUncertain { .. } => {
            if record.state == DiagnosticsJobState201::Pending {
                record.state = DiagnosticsJobState201::Uncertain;
            }
            false
        }
        CommandLifecycle::Rejected { .. } => {
            if record.state == DiagnosticsJobState201::Pending {
                record.state = DiagnosticsJobState201::NotSent;
            }
            false
        }
        CommandLifecycle::ProtocolResponse { .. } => {
            let reply = reply.expect("checked native reply");
            if reply.accepted() {
                retire_older(transaction, &record, &reply, incoming.recorded_at)?;
                if unanswered(record.state) {
                    record.state = DiagnosticsJobState201::Accepted;
                }
            } else if unanswered(record.state) {
                record.state = DiagnosticsJobState201::Rejected;
            }
            false
        }
    };
    if record.state != previous_state {
        record.changed_at = record.changed_at.max(incoming.recorded_at);
    }
    save(transaction, &record, Some(inflight))?;
    let job = evidence(&record);
    if let Some(result) = &mut incoming.diagnostics_201 {
        result.job = job;
    } else {
        incoming.diagnostics_201 = Some(DiagnosticsResult201 {
            log_type: record.log_type,
            request_id: record.native_request_id,
            destination: None,
            reply: None,
            job,
        });
    }
    incoming.schema_version = ContractVersion::V1_DIAGNOSTICS_201;
    Ok(())
}

/// An accepted request ends every older unresolved job of the station: the station either
/// cancelled it (`AcceptedCanceled`, N01.FR.12) or no longer had it in progress.
fn retire_older(
    transaction: &Transaction<'_>,
    record: &DiagnosticsJobRecord201,
    reply: &DiagnosticsReply201,
    at: uob_contracts::UtcTimestamp,
) -> Result<(), StorageError> {
    let cancelled = reply.cancelled_previous();
    let station = crate::snapshots::station_key(&record.station)?;
    for mut older in read_key(transaction, &station)? {
        if older.revision >= record.revision || older.state.resolved() {
            continue;
        }
        older.state = if cancelled {
            DiagnosticsJobState201::Cancelled
        } else {
            DiagnosticsJobState201::Superseded
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
    record: &DiagnosticsJobRecord201,
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
        if let Some(stored) = &mut result.diagnostics_201 {
            stored.job = evidence(record);
            transaction
                .execute(
                    "UPDATE command_results SET payload=?2 WHERE request_id=?1",
                    params![
                        record.request_id.as_str(),
                        serde_json::to_string(&result)
                            .map_err(|_| conflict("log result encoding failed"))?
                    ],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}
