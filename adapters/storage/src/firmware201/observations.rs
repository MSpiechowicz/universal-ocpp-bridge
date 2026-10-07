use super::{read_key, save, transitions::sync_result};
use rusqlite::Connection;
use uob_application::{
    FirmwareJobRecord201, FirmwareObservation201, FirmwareObservationKind201, StorageError,
    apply_firmware_status_201,
};
use uob_contracts::{FirmwareJobState201, FirmwareStatus201};

/// Attributes one native fact to at most one job; an unmatched fact changes nothing.
/// A report matches only its exact `requestId` (L01.FR.10). An identity-free `Idle`
/// (L01.FR.20) reports that the station has no firmware work in progress, which settles the
/// newest job the station has answered; a job still awaiting its reply is left alone.
pub(crate) fn observe(
    connection: &Connection,
    observation: &FirmwareObservation201,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&observation.station)?;
    let records = read_key(connection, &station)?;
    match observation.kind {
        FirmwareObservationKind201::Expiry => {
            for mut record in records {
                if record.state.resolved()
                    || matches!(
                        record.state,
                        FirmwareJobState201::Pending | FirmwareJobState201::TimedOut
                    )
                    || record.deadline > observation.observed_at
                {
                    continue;
                }
                record.state = FirmwareJobState201::TimedOut;
                record.changed_at = record.changed_at.max(observation.observed_at);
                save(connection, &record, None)?;
                sync_result(connection, &record)?;
            }
        }
        FirmwareObservationKind201::Status { status, request_id } => {
            let Some(mut record) = attributed(records, status, request_id) else {
                return Ok(());
            };
            apply_firmware_status_201(&mut record, status, observation.observed_at);
            save(connection, &record, None)?;
            sync_result(connection, &record)?;
        }
    }
    Ok(())
}

fn attributed(
    records: Vec<FirmwareJobRecord201>,
    status: FirmwareStatus201,
    request_id: Option<i32>,
) -> Option<FirmwareJobRecord201> {
    let mut newest_first = records.into_iter().rev();
    match request_id {
        Some(request_id) => newest_first.find(|record| record.native_request_id == request_id),
        None if status == FirmwareStatus201::Idle => newest_first.find(|record| {
            !record.state.resolved()
                && !matches!(
                    record.state,
                    FirmwareJobState201::Pending | FirmwareJobState201::Uncertain
                )
        }),
        None => None,
    }
}
