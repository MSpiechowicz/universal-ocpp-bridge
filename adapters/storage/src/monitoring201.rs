//! Durable OCPP 2.0.1 monitoring evidence. A `GetMonitoringReport` acknowledgement is recorded
//! first and its report is finished later; a terminal report is frozen against stale writers.
use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use uob_application::{MonitoringReportStore201, StorageError, StorageErrorCode, StorageFuture};
use uob_contracts::{
    CommandLifecycle, CommandResult, ContractVersion, DeviceReportFailure201,
    MONITORING_REPORT_OUTPUT_LIMIT_201, MonitoringOutcome201, MonitoringQuery201,
    MonitoringReportState201, MonitoringResult201, RequestId, UtcTimestamp,
};

fn integrity() -> StorageError {
    StorageError::new(
        StorageErrorCode::IntegrityFailure,
        "monitoring evidence identity changed",
    )
}

fn invalid(message: &'static str) -> StorageError {
    StorageError::new(StorageErrorCode::InvalidRequest, message)
}

/// Preserves the recorded request echo and native reply, and any terminal report, across later
/// writers.
pub(crate) fn merge(
    previous: &CommandResult,
    incoming: &mut CommandResult,
) -> Result<(), StorageError> {
    let Some(old) = previous.monitoring_201.as_ref() else {
        return Ok(());
    };
    let Some(new) = incoming.monitoring_201.as_mut() else {
        incoming.monitoring_201 = Some(old.clone());
        incoming.schema_version = ContractVersion::V1_MONITORING_201;
        return Ok(());
    };
    if old.query != new.query {
        return Err(integrity());
    }

    let frozen = match (&old.outcome, &mut new.outcome) {
        (
            MonitoringOutcome201::Report {
                ack: old_ack,
                report: old_report,
            },
            MonitoringOutcome201::Report { ack: new_ack, .. },
        ) => {
            if old_ack.is_some() && new_ack.is_some() && old_ack != new_ack {
                return Err(integrity());
            }
            // The acknowledgement is recorded once; a later writer may omit it.
            if new_ack.is_none() {
                *new_ack = *old_ack;
            }
            !old_report.pending()
        }
        (old_outcome, new_outcome) => {
            if old_outcome != &*new_outcome {
                return Err(integrity());
            }
            false
        }
    };
    if frozen {
        *new = old.clone();
    }
    incoming.schema_version = ContractVersion::V1_MONITORING_201;
    Ok(())
}

fn other_evidence(result: &CommandResult) -> bool {
    result.configuration.is_some()
        || !result.configuration_observations.is_empty()
        || result.configuration_201.is_some()
        || result.trigger_observation.is_some()
        || result.trigger_observation_201.is_some()
        || result.composite_schedule_16.is_some()
        || result.composite_schedule_201.is_some()
        || result.device_model_201.is_some()
        || result.charging_profile_16.is_some()
        || result.charging_profile_201.is_some()
        || result.charging_profiles_201.is_some()
        || result.local_authorization_16.is_some()
        || result.local_authorization_201.is_some()
        || result.reservation_16.is_some()
        || result.reservation_201.is_some()
        || result.firmware_16.is_some()
        || result.firmware_201.is_some()
        || result.diagnostics_16.is_some()
        || result.diagnostics_201.is_some()
}

/// Whether the native reply is the shape its request echo asks for.
fn corresponds(evidence: &MonitoringResult201) -> bool {
    match (&evidence.query, &evidence.outcome) {
        (
            MonitoringQuery201::SetVariableMonitoring { monitors },
            MonitoringOutcome201::SetMonitors { monitors: results },
        ) => monitors.len() == results.len(),
        (
            MonitoringQuery201::ClearVariableMonitoring { ids },
            MonitoringOutcome201::ClearMonitors { monitors: results },
        ) => {
            let mut requested = ids.clone();
            let mut answered = results.iter().map(|item| item.id).collect::<Vec<_>>();
            requested.sort_unstable();
            answered.sort_unstable();
            requested == answered
        }
        (MonitoringQuery201::SetMonitoringBase { .. }, MonitoringOutcome201::Base { .. })
        | (MonitoringQuery201::SetMonitoringLevel { .. }, MonitoringOutcome201::Level { .. })
        | (MonitoringQuery201::GetMonitoringReport { .. }, MonitoringOutcome201::Report { .. }) => {
            true
        }
        _ => false,
    }
}

/// Stateless shape of the evidence: its own revision, no other feature's evidence, and a native
/// reply that answers the request echo. Applied to every encoded and decoded result.
pub(crate) fn validate_result(result: &CommandResult) -> Result<(), StorageError> {
    let Some(evidence) = &result.monitoring_201 else {
        return Ok(());
    };
    if result.schema_version != ContractVersion::V1_MONITORING_201
        || other_evidence(result)
        || !corresponds(evidence)
    {
        return Err(invalid("monitoring evidence is inconsistent"));
    }
    Ok(())
}

struct Counter {
    bytes: usize,
}

impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|size| *size <= MONITORING_REPORT_OUTPUT_LIMIT_201)
            .ok_or_else(|| io::Error::other("bounded monitoring serialization"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fits(value: &impl Serialize) -> bool {
    serde_json::to_writer(Counter { bytes: 0 }, value).is_ok()
}

/// A complete report that cannot be retained within the bound becomes an explicit
/// `OutputLimit` failure; it is never silently truncated. No progress is invented because a
/// complete report carries no native byte count.
pub(crate) fn bound_output(result: &mut CommandResult) -> Result<(), StorageError> {
    if result.monitoring_201.is_none() || fits(result) {
        return Ok(());
    }

    if let Some(MonitoringResult201 {
        outcome: MonitoringOutcome201::Report { report, .. },
        ..
    }) = &mut result.monitoring_201
        && matches!(report, MonitoringReportState201::Complete { .. })
    {
        *report = MonitoringReportState201::Incomplete {
            reason: DeviceReportFailure201::OutputLimit,
            progress: None,
        };
    }

    if fits(result) {
        Ok(())
    } else {
        Err(invalid("monitoring result exceeds bounded output"))
    }
}

/// A stored complete report always fits the output bound.
pub(crate) fn validate_output(result: &CommandResult) -> Result<(), StorageError> {
    let Some(MonitoringResult201 {
        outcome:
            MonitoringOutcome201::Report {
                report: report @ MonitoringReportState201::Complete { .. },
                ..
            },
        ..
    }) = &result.monitoring_201
    else {
        return Ok(());
    };
    if fits(report) {
        Ok(())
    } else {
        Err(invalid("monitoring report exceeds bounded output"))
    }
}

pub(crate) fn pending(result: &CommandResult) -> bool {
    result
        .monitoring_201
        .as_ref()
        .is_some_and(MonitoringResult201::pending)
}

/// Startup terminalization of a report whose collector cannot have survived the restart.
pub(crate) fn interrupt(result: &mut CommandResult) {
    if let Some(MonitoringResult201 {
        outcome: MonitoringOutcome201::Report { report, .. },
        ..
    }) = &mut result.monitoring_201
        && report.pending()
    {
        *report = MonitoringReportState201::Incomplete {
            reason: DeviceReportFailure201::Interrupted,
            progress: None,
        };
    }
}

/// Records the finished report of one request, whichever of the collector and the application
/// writes first. A row that is still admitted or dispatched is resolved by the collector; a
/// pending report is replaced; a terminal report is returned untouched. An unknown request is
/// `None`.
pub(crate) fn finish(
    connection: &mut Connection,
    request: &str,
    evidence: MonitoringResult201,
    lifecycle: CommandLifecycle,
    now: UtcTimestamp,
) -> Result<Option<CommandResult>, StorageError> {
    if !matches!(
        &evidence.outcome,
        MonitoringOutcome201::Report { report, .. } if !report.pending()
    ) {
        return Err(invalid("a finished monitoring report must be terminal"));
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let Some(previous) = crate::recovery::command_result(&transaction, request)? else {
        return Ok(None);
    };
    let unresolved = matches!(
        previous.lifecycle,
        CommandLifecycle::Admitted | CommandLifecycle::Dispatched
    );
    let settled = previous.monitoring_201.as_ref().is_some_and(|stored| {
        matches!(&stored.outcome, MonitoringOutcome201::Report { report, .. } if !report.pending())
    });
    // A terminal command without monitoring evidence (for example, already uncertain) stays as
    // recorded, and a terminal report is never replaced.
    if settled || (previous.monitoring_201.is_none() && !unresolved) {
        return Ok(Some(previous));
    }

    let mut incoming = previous.clone();
    if unresolved {
        incoming.lifecycle = lifecycle;
    }
    incoming.monitoring_201 = Some(evidence);
    incoming.schema_version = ContractVersion::V1_MONITORING_201;
    incoming.recorded_at = now;
    merge(&previous, &mut incoming)?;
    crate::command::write_result_value(&transaction, incoming, request)?;
    let result = crate::recovery::command_result(&transaction, request)?;
    transaction.commit().map_err(unavailable)?;
    Ok(result)
}

impl<C, E, D, R> MonitoringReportStore201 for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: DeserializeOwned + Send + Sync + 'static,
    D: DeserializeOwned + Send + Sync + 'static,
    R: DeserializeOwned + Send + Sync + 'static,
{
    fn finish_monitoring_report(
        &self,
        request: RequestId,
        evidence: MonitoringResult201,
        lifecycle: CommandLifecycle,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        self.request(|reply| {
            Request::MonitoringReport(
                request.as_str().to_owned(),
                Box::new(evidence),
                lifecycle,
                now,
                reply,
            )
        })
    }
}
