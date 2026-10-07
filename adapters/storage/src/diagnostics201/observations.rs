use super::{read_key, save, transitions::sync_result};
use rusqlite::Connection;
use uob_application::{
    DiagnosticsObservation201, DiagnosticsObservationKind201, StorageError,
    apply_diagnostics_status_201, attribute_diagnostics_201,
};
use uob_contracts::DiagnosticsJobState201;

/// Attributes one native fact to at most one job; an unmatched fact changes nothing.
pub(crate) fn observe(
    connection: &Connection,
    observation: &DiagnosticsObservation201,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&observation.station)?;
    let mut records = read_key(connection, &station)?;
    match &observation.kind {
        DiagnosticsObservationKind201::Expiry => {
            for mut record in records {
                if record.state.resolved()
                    || matches!(
                        record.state,
                        DiagnosticsJobState201::Pending | DiagnosticsJobState201::TimedOut
                    )
                    || record.deadline > observation.observed_at
                {
                    continue;
                }
                record.state = DiagnosticsJobState201::TimedOut;
                record.changed_at = record.changed_at.max(observation.observed_at);
                save(connection, &record, None)?;
                sync_result(connection, &record)?;
            }
        }
        DiagnosticsObservationKind201::Status {
            status,
            request_id,
            upload,
        } => {
            let Some(index) = attribute_diagnostics_201(&records, *status, *request_id) else {
                return Ok(());
            };
            let record = &mut records[index];
            apply_diagnostics_status_201(record, *status, upload.as_ref(), observation.observed_at);
            save(connection, record, None)?;
            sync_result(connection, record)?;
        }
    }
    Ok(())
}
