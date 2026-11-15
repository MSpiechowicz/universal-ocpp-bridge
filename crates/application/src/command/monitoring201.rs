//! Native OCPP 2.0.1 monitoring command outcome and the report-completion persistence port.
use crate::{FlowEvidence, StorageFuture};
use uob_contracts::{
    CommandError, CommandErrorCode, CommandLifecycle, CommandResult, MonitoringResult201,
    RequestId, UtcTimestamp,
};

/// Report collection outlives an HTTP response future. Implementations merge atomically: the
/// first writer of a terminal report wins, and the application's own later write of the pending
/// acknowledgement adopts it instead of replacing it.
pub trait MonitoringReportStore201: Send + Sync {
    /// Records the finished `GetMonitoringReport` evidence (query, native acknowledgement and
    /// terminal report) for one request, whether or not the application has recorded the
    /// acknowledgement yet. `lifecycle` resolves a command that is still admitted or dispatched.
    ///
    /// Returns the stored result. An already terminal report is returned untouched, so a stored
    /// report that differs from the offered one tells the caller it lost the race. `None` means
    /// the request is unknown.
    fn finish_monitoring_report(
        &self,
        request: RequestId,
        evidence: MonitoringResult201,
        lifecycle: CommandLifecycle,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>>;
}

/// Lifecycle a native `GetMonitoringReport` acknowledgement resolves its command to, for a
/// collector that finishes before the application records the acknowledgement.
#[must_use]
pub fn monitoring_response_lifecycle(evidence: &MonitoringResult201) -> CommandLifecycle {
    lifecycle(evidence).0
}

/// Lifecycle of a native reply: accepted only when the charger accepted everything it was
/// asked to do. A still-pending report keeps the lifecycle of its acknowledgement.
pub(super) fn lifecycle(evidence: &MonitoringResult201) -> (CommandLifecycle, FlowEvidence) {
    if evidence.accepted() {
        return (
            CommandLifecycle::ProtocolResponse {
                accepted: true,
                error: None,
            },
            FlowEvidence::Accepted,
        );
    }

    (
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: Some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                detail: None,
            }),
        },
        FlowEvidence::Rejected,
    )
}
