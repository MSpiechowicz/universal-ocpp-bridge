mod output;
mod staging;
use crate::{SqliteOperationalStore, codec, configuration::unavailable, worker::Request};
pub(crate) use output::bound_output;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{DeviceModelStore201, StorageError, StorageErrorCode, StorageFuture};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, DeviceModelResult201, DeviceReportAck201,
    DeviceReportFailure201, DeviceReportState201, RequestId, UtcTimestamp,
};

fn accepted_progress(
    report: &DeviceReportState201,
) -> Option<uob_contracts::DeviceReportProgress201> {
    match report {
        DeviceReportState201::Complete { progress, .. } => Some(*progress),
        DeviceReportState201::Incomplete { progress, .. } => *progress,
        _ => None,
    }
}
fn enrichable(report: &DeviceReportState201) -> bool {
    matches!(
        report,
        DeviceReportState201::Incomplete {
            reason: DeviceReportFailure201::MissingAcknowledgement
                | DeviceReportFailure201::NativeRejected
                | DeviceReportFailure201::NotTransmitted
                | DeviceReportFailure201::Disconnected
                | DeviceReportFailure201::Capacity
                | DeviceReportFailure201::StorageUnavailable,
            progress: None
        }
    )
}
fn integrity() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "device-model evidence identity changed",
    )
}

pub(crate) fn merge(
    previous: &mut CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = previous.device_model_201.take() else {
        return Ok(());
    };
    let Some(new) = incoming.device_model_201.as_mut() else {
        incoming.device_model_201 = Some(old);
        incoming.schema_version = ContractVersion::V1_DEVICE_MODEL_201;
        return Ok(());
    };
    if old.query != new.query
        || old.connection != new.connection
        || old.generation != new.generation
        || old.dispatch_recorded_at != new.dispatch_recorded_at
    {
        return Err(integrity());
    }
    if let Some(ack) = old.native_ack {
        if new.native_ack.is_some_and(|value| value != ack) {
            return Err(integrity());
        }
        new.native_ack = Some(ack);
    }
    if !old.variables.is_empty() {
        if !new.variables.is_empty() && old.variables != new.variables {
            return Err(integrity());
        }
        new.variables = old.variables;
    }
    if !old.report.pending() {
        let progress = if enrichable(&old.report) {
            accepted_progress(&new.report)
        } else {
            None
        };
        new.report = old.report;
        if let Some(progress) = progress
            && let DeviceReportState201::Incomplete {
                progress: saved, ..
            } = &mut new.report
        {
            *saved = Some(progress);
        }
    }
    incoming.schema_version = ContractVersion::V1_DEVICE_MODEL_201;
    Ok(())
}

pub(crate) fn finalize_lifecycle(result: &mut CommandResult) {
    let Some(evidence) = result.device_model_201.as_mut() else {
        return;
    };
    if evidence.query.request_id().is_none() {
        return;
    }
    let failure = match evidence.native_ack {
        Some(DeviceReportAck201::Rejected | DeviceReportAck201::NotSupported) => {
            Some(DeviceReportFailure201::NativeRejected)
        }
        Some(DeviceReportAck201::EmptyResultSet) => {
            evidence.report = DeviceReportState201::NotExpected;
            return;
        }
        Some(DeviceReportAck201::Accepted) => None,
        None => match result.lifecycle {
            CommandLifecycle::Rejected { .. } => Some(DeviceReportFailure201::NotTransmitted),
            CommandLifecycle::TransmissionUncertain { .. } => {
                Some(DeviceReportFailure201::MissingAcknowledgement)
            }
            CommandLifecycle::ProtocolResponse {
                accepted: false, ..
            } => Some(DeviceReportFailure201::NativeRejected),
            _ if matches!(evidence.report, DeviceReportState201::Complete { .. }) => {
                Some(DeviceReportFailure201::MissingAcknowledgement)
            }
            _ => None,
        },
    };
    if let Some(reason) = failure {
        if matches!(
            evidence.report,
            DeviceReportState201::Incomplete { .. } | DeviceReportState201::NotExpected
        ) {
            return;
        }
        let progress = match &evidence.report {
            DeviceReportState201::Complete { progress, .. } => Some(*progress),
            DeviceReportState201::Incomplete { progress, .. } => *progress,
            _ => None,
        };
        evidence.report = DeviceReportState201::Incomplete { reason, progress };
    }
}

pub(crate) fn finish(
    connection: &mut Connection,
    request: &str,
    mut evidence: DeviceModelResult201,
    lifecycle: Option<CommandLifecycle>,
    now: UtcTimestamp,
) -> Result<Option<CommandResult>, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let Some(mut result) = crate::recovery::command_result(&transaction, request)? else {
        return Ok(None);
    };
    let old = result.device_model_201.as_ref().ok_or_else(integrity)?;
    let terminal = !old.report.pending();
    let enrich = enrichable(&old.report) && accepted_progress(&evidence.report).is_some();
    staging::prepare(
        &transaction,
        request,
        old,
        &mut evidence,
        lifecycle.as_ref().unwrap_or(&result.lifecycle),
    )?;
    if terminal && lifecycle.is_none() && !enrich {
        return Ok(Some(result));
    }
    result.device_model_201 = Some(evidence);
    result.recorded_at = now;
    if let Some(lifecycle) = lifecycle {
        result.lifecycle = lifecycle;
    }
    crate::command::write_result_value(&transaction, result, request)?;
    let result = crate::recovery::command_result(&transaction, request)?;
    transaction.commit().map_err(unavailable)?;
    Ok(result)
}

/// Terminalize bounded indexed pages at explicit startup, including terminal native lifecycles.
/// This deliberately never constructs a replay request or invents recovered progress.
pub(crate) fn interrupt(connection: &Connection) -> Result<(), StorageError> {
    loop {
        let mut statement = connection.prepare("SELECT request_id, payload FROM command_results INDEXED BY device_report_pending WHERE report_pending = 1 ORDER BY request_id LIMIT 1").map_err(unavailable)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(unavailable)?;
        let rows = rows.collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
        drop(statement);
        if rows.is_empty() {
            return Ok(());
        }
        let transaction = connection.unchecked_transaction().map_err(unavailable)?;
        for (id, payload) in rows {
            let mut result = codec::decode_result(&payload)?;
            let evidence = result.device_model_201.as_mut().ok_or_else(integrity)?;
            evidence.report = DeviceReportState201::Incomplete {
                reason: DeviceReportFailure201::Interrupted,
                progress: None,
            };
            let payload = serde_json::to_string(&result).map_err(|_| integrity())?;
            transaction.execute("UPDATE command_results SET payload = ?2, report_pending = 0 WHERE request_id = ?1 AND report_pending = 1", params![id, payload]).map_err(unavailable)?;
            transaction
                .execute(
                    "DELETE FROM device_report_staging WHERE request_id = ?1",
                    [&id],
                )
                .map_err(unavailable)?;
        }
        transaction.commit().map_err(unavailable)?;
    }
}

impl<C, E, D, R> DeviceModelStore201 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn interrupt_device_reports(&self) -> StorageFuture<'_, ()> {
        self.request(Request::InterruptDeviceReports)
    }
    fn device_model_result(&self, request: RequestId) -> StorageFuture<'_, Option<CommandResult>> {
        self.request(|reply| Request::CommandResult(request.as_str().to_owned(), reply))
    }
    fn finish_device_report(
        &self,
        request: RequestId,
        evidence: DeviceModelResult201,
        lifecycle: Option<CommandLifecycle>,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        self.request(|reply| {
            Request::DeviceReport(request.as_str().to_owned(), evidence, lifecycle, now, reply)
        })
    }
}
