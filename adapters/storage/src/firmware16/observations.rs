use super::{read_key, save, transitions::sync_result};
use rusqlite::Connection;
use uob_application::{
    FirmwareJobRecord16, FirmwareObservation16, FirmwareObservationKind16, FirmwareVariant16,
    StorageError, apply_firmware_status_16,
};
use uob_contracts::{FirmwareJobState16, FirmwareStatus16};

/// Attributes one native fact to at most one job; an unmatched fact changes nothing.
/// Legacy notifications carry no identity, so they belong to the newest unresolved legacy job.
/// Signed notifications match only their exact `requestId` (L01.FR.10); an identity-free
/// `Idle` (L01.FR.21) reports that the station has no firmware work in progress.
pub(crate) fn observe(
    connection: &Connection,
    observation: &FirmwareObservation16,
) -> Result<(), StorageError> {
    let station = crate::snapshots::station_key(&observation.station)?;
    let records = read_key(connection, &station)?;
    match observation.kind {
        FirmwareObservationKind16::Expiry => {
            for mut record in records {
                if record.state.resolved()
                    || matches!(
                        record.state,
                        FirmwareJobState16::Pending | FirmwareJobState16::TimedOut
                    )
                    || record.deadline > observation.observed_at
                {
                    continue;
                }
                record.state = FirmwareJobState16::TimedOut;
                record.changed_at = record.changed_at.max(observation.observed_at);
                save(connection, &record, None)?;
                sync_result(connection, &record)?;
            }
        }
        FirmwareObservationKind16::Status {
            status,
            signed,
            request_id,
        } => {
            let Some(mut record) = attributed(records, status, signed, request_id) else {
                return Ok(());
            };
            apply_firmware_status_16(&mut record, status, observation.observed_at);
            save(connection, &record, None)?;
            sync_result(connection, &record)?;
        }
    }
    Ok(())
}

fn attributed(
    records: Vec<FirmwareJobRecord16>,
    status: FirmwareStatus16,
    signed: bool,
    request_id: Option<i32>,
) -> Option<FirmwareJobRecord16> {
    let mut newest_first = records.into_iter().rev();
    match (signed, request_id) {
        (false, _) => newest_first
            .find(|record| record.variant == FirmwareVariant16::Legacy && !record.state.resolved()),
        (true, Some(request_id)) => {
            newest_first.find(|record| record.variant == FirmwareVariant16::Signed { request_id })
        }
        (true, None) if status == FirmwareStatus16::Idle => newest_first.find(|record| {
            matches!(record.variant, FirmwareVariant16::Signed { .. }) && !record.state.resolved()
        }),
        (true, None) => None,
    }
}
