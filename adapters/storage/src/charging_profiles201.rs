//! Durable OCPP 2.0.1 installed-profile evidence: the native acknowledgement is recorded first,
//! and a terminal report is frozen against stale or conflicting writers.
use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use uob_application::{
    ChargingProfileReportStore201, StorageError, StorageErrorCode, StorageFuture,
};
use uob_contracts::{
    CHARGING_PROFILE_REPORT_OUTPUT_LIMIT_201, ChargingProfileReportFailure201,
    ChargingProfileReportState201, ChargingProfilesResult201, ChargingProfilesStatus201,
    CommandLifecycle, CommandResult, ContractVersion, RequestId, UtcTimestamp,
};

fn integrity() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "installed-profile evidence identity changed",
    )
}

/// Preserves the recorded acknowledgement and any terminal report across later writers.
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = previous.charging_profiles_201.as_ref() else {
        return Ok(());
    };
    let Some(new) = incoming.charging_profiles_201.as_mut() else {
        incoming.charging_profiles_201 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_SCHEDULES_201;
        return Ok(());
    };
    if old.query != new.query || old.status != new.status || old.reason_code != new.reason_code {
        return Err(integrity());
    }
    if !old.report.pending() {
        new.report = old.report.clone();
    }
    incoming.schema_version = ContractVersion::V1_SCHEDULES_201;
    Ok(())
}

/// Evidence exists only beside its native acknowledgement, and the report follows its status.
pub(crate) fn validate_stored(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.charging_profiles_201 else {
        return Ok(());
    };
    let shaped = match evidence.status {
        ChargingProfilesStatus201::Accepted => {
            !matches!(evidence.report, ChargingProfileReportState201::NotExpected)
        }
        ChargingProfilesStatus201::NoProfiles => {
            matches!(evidence.report, ChargingProfileReportState201::NotExpected)
        }
    };
    if !shaped
        || result.schema_version != ContractVersion::V1_SCHEDULES_201
        || !matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { .. })
    {
        return Err(integrity());
    }
    Ok(())
}

struct Counter(usize);
impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|size| *size <= CHARGING_PROFILE_REPORT_OUTPUT_LIMIT_201)
            .ok_or_else(|| io::Error::other("bounded installed-profile serialization"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A report that cannot be retained within the bound becomes an explicit output failure.
pub(crate) fn bound_output(result: &mut CommandResult) {
    let Some(evidence) = &mut result.charging_profiles_201 else {
        return;
    };
    if let ChargingProfileReportState201::Complete { progress, .. } = &evidence.report
        && serde_json::to_writer(Counter(0), &evidence.report).is_err()
    {
        evidence.report = ChargingProfileReportState201::Incomplete {
            reason: ChargingProfileReportFailure201::OutputLimit,
            progress: Some(*progress),
        };
    }
}

pub(crate) fn pending(result: &CommandResult) -> bool {
    result
        .charging_profiles_201
        .as_ref()
        .is_some_and(|evidence| evidence.report.pending())
}

/// Startup terminalization of a report whose collector cannot have survived the restart.
pub(crate) fn interrupt(result: &mut CommandResult) -> Result<(), StorageError> {
    let evidence = result
        .charging_profiles_201
        .as_mut()
        .ok_or_else(integrity)?;
    if evidence.report.pending() {
        evidence.report = ChargingProfileReportState201::Incomplete {
            reason: ChargingProfileReportFailure201::Interrupted,
            progress: None,
        };
    }
    Ok(())
}

pub(crate) fn finish(
    connection: &mut Connection,
    request: &str,
    evidence: ChargingProfilesResult201,
    lifecycle: Option<CommandLifecycle>,
    now: UtcTimestamp,
) -> Result<Option<CommandResult>, StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let Some(mut result) = crate::recovery::command_result(&transaction, request)? else {
        return Ok(None);
    };
    let unresolved = matches!(
        result.lifecycle,
        CommandLifecycle::Admitted | CommandLifecycle::Dispatched
    );
    match lifecycle {
        // Only the native acknowledgement resolves the dispatched command.
        Some(lifecycle) if unresolved => result.lifecycle = lifecycle,
        // A terminal result without evidence (for example, already uncertain) stays as recorded.
        Some(_) if result.charging_profiles_201.is_none() => return Ok(Some(result)),
        None if result.charging_profiles_201.is_none() => return Err(integrity()),
        _ => {}
    }
    result.charging_profiles_201 = Some(evidence);
    result.schema_version = ContractVersion::V1_SCHEDULES_201;
    result.recorded_at = now;
    crate::command::write_result_value(&transaction, result, request)?;
    let result = crate::recovery::command_result(&transaction, request)?;
    transaction.commit().map_err(unavailable)?;
    Ok(result)
}

impl<C, E, D, R> ChargingProfileReportStore201 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn finish_charging_profiles(
        &self,
        request: RequestId,
        evidence: ChargingProfilesResult201,
        lifecycle: Option<CommandLifecycle>,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        self.request(|reply| {
            Request::ChargingProfilesReport(
                request.as_str().to_owned(),
                Box::new(evidence),
                lifecycle,
                now,
                reply,
            )
        })
    }
}
