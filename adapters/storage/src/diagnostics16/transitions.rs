use super::{conflict, read_key, save};
use crate::configuration::unavailable;
use rusqlite::{OptionalExtension, Transaction, params};
use uob_application::{DiagnosticsJobRecord16, DiagnosticsVariant16, StorageError};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, DiagnosticsJob16, DiagnosticsJobState16,
    DiagnosticsReply16, DiagnosticsResult16, GetLogStatus16,
};

pub(super) fn evidence(record: &DiagnosticsJobRecord16) -> DiagnosticsJob16 {
    DiagnosticsJob16 {
        revision: record.revision,
        state: record.state,
        deadline: record.deadline,
        observed_at: record.changed_at,
        last_status: record.last_status,
        last_status_at: record.last_status_at,
        notifications: record.notifications,
        rejected_transitions: record.rejected_transitions,
        upload: record.upload.clone(),
    }
}

const fn unanswered(state: DiagnosticsJobState16) -> bool {
    matches!(
        state,
        DiagnosticsJobState16::Pending | DiagnosticsJobState16::Uncertain
    )
}

/// Applies a command lifecycle to its durable job and fills the public job evidence.
pub(crate) fn finish(
    transaction: &Transaction<'_>,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let record: Option<(String, bool)> = transaction
        .query_row(
            "SELECT payload,inflight FROM diagnostics16_jobs WHERE request_id=?1",
            [incoming.return_route.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((payload, current_inflight)) = record else {
        if incoming.diagnostics_16.is_some() {
            return Err(conflict("diagnostics reply lacks durable job owner"));
        }
        return Ok(());
    };
    let mut record = crate::codec_diagnostics16::decode(&payload)?;
    super::validation::correlate(transaction, incoming)?;
    let previous_state = record.state;
    let reply = incoming
        .diagnostics_16
        .as_ref()
        .and_then(DiagnosticsResult16::reply)
        .cloned();
    if matches!(
        incoming.lifecycle,
        CommandLifecycle::ProtocolResponse { .. }
    ) != reply.is_some()
    {
        return Err(conflict("diagnostics protocol response lacks native reply"));
    }
    let inflight = match &incoming.lifecycle {
        CommandLifecycle::Admitted => current_inflight,
        CommandLifecycle::Dispatched => {
            record.started = true;
            current_inflight
        }
        CommandLifecycle::TransmissionUncertain { .. } => {
            if record.state == DiagnosticsJobState16::Pending {
                record.state = DiagnosticsJobState16::Uncertain;
            }
            false
        }
        CommandLifecycle::Rejected { .. } => {
            if record.state == DiagnosticsJobState16::Pending {
                record.state = DiagnosticsJobState16::NotSent;
            }
            false
        }
        CommandLifecycle::ProtocolResponse { .. } => {
            let reply = reply.expect("checked native reply");
            if reply.accepted() {
                retire_older(transaction, &record, &reply, incoming.recorded_at)?;
                if unanswered(record.state) {
                    record.state = DiagnosticsJobState16::Accepted;
                }
            } else if unanswered(record.state) {
                record.state = match reply {
                    // §5.9: no file name means the station has no diagnostics to upload.
                    DiagnosticsReply16::Diagnostics { file_name: None } => {
                        DiagnosticsJobState16::NoLogAvailable
                    }
                    _ => DiagnosticsJobState16::Rejected,
                };
            }
            false
        }
    };
    if record.state != previous_state {
        record.changed_at = record.changed_at.max(incoming.recorded_at);
    }
    save(transaction, &record, Some(inflight))?;
    let job = evidence(&record);
    if let Some(result) = &mut incoming.diagnostics_16 {
        *result.job_mut() = job;
    } else {
        incoming.diagnostics_16 = Some(match record.variant {
            DiagnosticsVariant16::Diagnostics => DiagnosticsResult16::GetDiagnostics {
                destination: None,
                reply: None,
                job,
            },
            DiagnosticsVariant16::Log {
                log_type,
                request_id,
            } => DiagnosticsResult16::GetLog {
                log_type,
                request_id,
                destination: None,
                reply: None,
                job,
            },
        });
    }
    incoming.schema_version = ContractVersion::V1_DIAGNOSTICS_16;
    Ok(())
}

/// An accepted request ends every older unresolved job of the station: the station either
/// cancelled it (`AcceptedCanceled`, N01.FR.11) or no longer had it in progress.
fn retire_older(
    transaction: &Transaction<'_>,
    record: &DiagnosticsJobRecord16,
    reply: &DiagnosticsReply16,
    at: uob_contracts::UtcTimestamp,
) -> Result<(), StorageError> {
    let cancelled = matches!(
        reply,
        DiagnosticsReply16::Log {
            status: GetLogStatus16::AcceptedCanceled,
            ..
        }
    );
    let station = crate::snapshots::station_key(&record.station)?;
    for mut older in read_key(transaction, &station)? {
        if older.revision >= record.revision || older.state.resolved() {
            continue;
        }
        older.state = if cancelled {
            DiagnosticsJobState16::Cancelled
        } else {
            DiagnosticsJobState16::Superseded
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
    record: &DiagnosticsJobRecord16,
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
        if let Some(evidence_slot) = &mut result.diagnostics_16 {
            *evidence_slot.job_mut() = evidence(record);
            transaction
                .execute(
                    "UPDATE command_results SET payload=?2 WHERE request_id=?1",
                    params![
                        record.request_id.as_str(),
                        serde_json::to_string(&result)
                            .map_err(|_| conflict("diagnostics result encoding failed"))?
                    ],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}
