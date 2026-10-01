use super::{integrity, output};
use crate::configuration::unavailable;
use rusqlite::{OptionalExtension, Transaction, params};
use uob_application::StorageError;
use uob_contracts::{
    CommandLifecycle, DeviceModelResult201, DeviceReportAck201, DeviceReportFailure201,
    DeviceReportProgress201, DeviceReportState201,
};

pub(super) fn prepare(
    transaction: &Transaction<'_>,
    request: &str,
    old: &DeviceModelResult201,
    incoming: &mut DeviceModelResult201,
    lifecycle: &CommandLifecycle,
) -> Result<(), StorageError> {
    if old.query != incoming.query
        || old.connection != incoming.connection
        || old.generation != incoming.generation
        || old.dispatch_recorded_at != incoming.dispatch_recorded_at
    {
        return Err(integrity());
    }
    if let Some(ack) = old.native_ack
        && incoming.native_ack.is_some_and(|value| value != ack)
    {
        return Err(integrity());
    }
    if super::enrichable(&old.report)
        && let Some(progress) = super::accepted_progress(&incoming.report)
        && let DeviceReportState201::Incomplete { reason, .. } = old.report
    {
        incoming.report = DeviceReportState201::Incomplete {
            reason,
            progress: Some(progress),
        };
    }
    if old.report.pending()
        && let DeviceReportState201::Incomplete {
            progress: saved @ None,
            ..
        } = &mut incoming.report
    {
        *saved = staged_progress(transaction, request)?;
    }
    let ack = old.native_ack.or(incoming.native_ack);
    if old.report.pending()
        && ack.is_none()
        && matches!(
            lifecycle,
            CommandLifecycle::Admitted | CommandLifecycle::Dispatched
        )
        && let DeviceReportState201::Complete { progress, .. } = &incoming.report
    {
        if !output::fits(&incoming.report) {
            return Err(integrity());
        }
        let payload = serde_json::to_string(&incoming.report).map_err(|_| integrity())?;
        transaction.execute(
            "INSERT INTO device_report_staging(request_id,payload,accepted_fragments,accepted_items,accepted_bytes)
             VALUES (?1,?2,?3,?4,?5) ON CONFLICT(request_id) DO NOTHING",
            params![request, payload, i64::from(progress.fragments),
                i64::try_from(progress.items).map_err(|_| integrity())?,
                i64::try_from(progress.bytes).map_err(|_| integrity())?],
        ).map_err(unavailable)?;
        incoming.report = DeviceReportState201::Pending;
    }
    if old.report.pending()
        && incoming.report.pending()
        && ack == Some(DeviceReportAck201::Accepted)
    {
        let payload: Option<String> = transaction
            .query_row(
                "SELECT payload FROM device_report_staging WHERE request_id=?1",
                [request],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        if let Some(payload) = payload {
            incoming.report = serde_json::from_str(&payload).map_err(|_| integrity())?;
            if !matches!(incoming.report, DeviceReportState201::Complete { .. }) {
                return Err(integrity());
            }
        }
    }
    let failure = report_failure(ack, lifecycle);
    if old.report.pending()
        && incoming.report.pending()
        && let Some(reason) = failure
    {
        let progress = staged_progress(transaction, request)?;
        incoming.report = DeviceReportState201::Incomplete { reason, progress };
    }
    if !incoming.report.pending()
        || failure.is_some()
        || ack == Some(DeviceReportAck201::EmptyResultSet)
    {
        transaction
            .execute(
                "DELETE FROM device_report_staging WHERE request_id=?1",
                [request],
            )
            .map_err(unavailable)?;
    }
    Ok(())
}

fn report_failure(
    ack: Option<DeviceReportAck201>,
    lifecycle: &CommandLifecycle,
) -> Option<DeviceReportFailure201> {
    match ack {
        Some(DeviceReportAck201::Rejected | DeviceReportAck201::NotSupported) => {
            Some(DeviceReportFailure201::NativeRejected)
        }
        Some(DeviceReportAck201::EmptyResultSet | DeviceReportAck201::Accepted) => None,
        None => match lifecycle {
            CommandLifecycle::TransmissionUncertain { .. } => {
                Some(DeviceReportFailure201::MissingAcknowledgement)
            }
            CommandLifecycle::Rejected { .. } => Some(DeviceReportFailure201::NotTransmitted),
            CommandLifecycle::ProtocolResponse {
                accepted: false, ..
            } => Some(DeviceReportFailure201::NativeRejected),
            _ => None,
        },
    }
}

fn staged_progress(
    transaction: &Transaction<'_>,
    request: &str,
) -> Result<Option<DeviceReportProgress201>, StorageError> {
    let row: Option<(i64, i64, i64)> = transaction.query_row(
        "SELECT accepted_fragments,accepted_items,accepted_bytes FROM device_report_staging WHERE request_id=?1",
        [request], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(unavailable)?;
    row.map(|(fragments, items, bytes)| {
        let progress = DeviceReportProgress201 {
            fragments: u32::try_from(fragments).map_err(|_| integrity())?,
            items: usize::try_from(items).map_err(|_| integrity())?,
            bytes: usize::try_from(bytes).map_err(|_| integrity())?,
        };
        if progress.fragments > 256
            || progress.items > 4096
            || progress.bytes > uob_contracts::DEVICE_MODEL_OUTPUT_LIMIT_201
        {
            return Err(integrity());
        }
        Ok(progress)
    })
    .transpose()
}
