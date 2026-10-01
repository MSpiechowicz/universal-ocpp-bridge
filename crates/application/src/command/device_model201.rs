//! Narrow persistence port for connection-owned report completion.
use crate::StorageFuture;
use uob_contracts::{CommandResult, DeviceModelResult201, RequestId, UtcTimestamp};

/// Report collection outlives an HTTP response future. Implementations merge atomically.
pub trait DeviceModelStore201: Send + Sync {
    fn device_model_result(&self, request: RequestId) -> StorageFuture<'_, Option<CommandResult>>;
    /// Startup-only reconciliation, before any connection is admitted; never replay.
    fn interrupt_device_reports(&self) -> StorageFuture<'_, ()>;
    fn finish_device_report(
        &self,
        request: RequestId,
        evidence: DeviceModelResult201,
        lifecycle: Option<uob_contracts::CommandLifecycle>,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>>;
}
