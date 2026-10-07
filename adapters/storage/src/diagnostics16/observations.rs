use super::{read_key, save, transitions::sync_result};
use rusqlite::Connection;
use uob_application::{
    DiagnosticsObservation16, DiagnosticsObservationKind16, StorageError,
    apply_diagnostics_status_16, attribute_diagnostics_16,
};
use uob_contracts::DiagnosticsJobState16;

/// Attributes one native fact to at most one job; an unmatched fact changes nothing.
pub(crate) fn observe(
    connection: &Connection,
    observation: &DiagnosticsObservation16,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&observation.station)?;
    let mut records = read_key(connection, &station)?;
    match &observation.kind {
        DiagnosticsObservationKind16::Expiry => {
            for mut record in records {
                if record.state.resolved()
                    || matches!(
                        record.state,
                        DiagnosticsJobState16::Pending | DiagnosticsJobState16::TimedOut
                    )
                    || record.deadline > observation.observed_at
                {
                    continue;
                }
                record.state = DiagnosticsJobState16::TimedOut;
                record.changed_at = record.changed_at.max(observation.observed_at);
                save(connection, &record, None)?;
                sync_result(connection, &record)?;
            }
        }
        DiagnosticsObservationKind16::Status {
            status,
            log,
            request_id,
            upload,
        } => {
            let Some(index) = attribute_diagnostics_16(&records, *status, *log, *request_id) else {
                return Ok(());
            };
            let record = &mut records[index];
            apply_diagnostics_status_16(record, *status, upload.as_ref(), observation.observed_at);
            save(connection, record, None)?;
            sync_result(connection, record)?;
        }
    }
    Ok(())
}
