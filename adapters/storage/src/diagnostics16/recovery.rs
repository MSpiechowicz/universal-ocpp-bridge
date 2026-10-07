use super::{observe, read_key, release_job_id, save, transitions::sync_result};
use crate::{configuration::unavailable, firmware16::register_release_job};
use rusqlite::{Connection, TransactionBehavior};
use uob_application::{DiagnosticsObservation16, DiagnosticsObservationKind16, StorageError};
use uob_contracts::{DiagnosticsJobState16, UtcTimestamp};

/// Startup clears in-flight ownership without replaying: an unsent job is `not_sent`, a
/// possibly delivered one `uncertain`. Every unresolved job keeps (or regains) its release-drain
/// registration. Both modes then apply the trusted deadline.
pub(crate) fn maintain(
    connection: &mut Connection,
    now: UtcTimestamp,
    startup: bool,
) -> Result<(), StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let stations = {
        let mut statement = transaction
            .prepare("SELECT DISTINCT station FROM diagnostics16_jobs")
            .map_err(unavailable)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?
    };
    for station in stations {
        let records = read_key(&transaction, &station)?;
        if startup {
            transaction
                .execute(
                    "UPDATE diagnostics16_jobs SET inflight=0 WHERE station=?1",
                    [&station],
                )
                .map_err(unavailable)?;
            for mut record in records.iter().filter(|r| !r.state.resolved()).cloned() {
                if record.state == DiagnosticsJobState16::Pending {
                    record.state = if record.started {
                        DiagnosticsJobState16::Uncertain
                    } else {
                        DiagnosticsJobState16::NotSent
                    };
                    record.changed_at = record.changed_at.max(now);
                }
                if !record.state.resolved() {
                    register_release_job(
                        &transaction,
                        &release_job_id(&record.request_id),
                        "diagnostics",
                        false,
                    )?;
                }
                save(&transaction, &record, Some(false))?;
                sync_result(&transaction, &record)?;
            }
        }
        if let Some(record) = records.first() {
            observe(
                &transaction,
                &DiagnosticsObservation16 {
                    station: record.station.clone(),
                    observed_at: now,
                    kind: DiagnosticsObservationKind16::Expiry,
                },
            )?;
        }
    }
    transaction.commit().map_err(unavailable)
}
