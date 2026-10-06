//! Narrow persistence port for connection-owned installed-profile report completion.
use crate::StorageFuture;
use uob_contracts::{
    ChargingProfilesResult201, CommandLifecycle, CommandResult, RequestId, UtcTimestamp,
};

/// The report owner outlives an HTTP response future. Implementations merge atomically and
/// never let a later write replace a terminal report.
pub trait ChargingProfileReportStore201: Send + Sync {
    /// Records the native acknowledgement, then later the collected report, for one request.
    fn finish_charging_profiles(
        &self,
        request: RequestId,
        evidence: ChargingProfilesResult201,
        lifecycle: Option<CommandLifecycle>,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>>;
}
