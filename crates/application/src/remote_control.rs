//! Durable native correlation and safe response evidence for OCPP 2.0.1 commands.
use crate::StorageFuture;
use uob_contracts::RequestId;

pub use uob_contracts::RemoteControlEvidence;

/// One bounded row per retained command; deleted with command retention, while allocated IDs
/// are never reused. Implementations must fail closed on exhaustion or missing durable commands.
pub trait RemoteControlStore: Send + Sync {
    /// Atomically returns an existing allocation or persists a globally unique positive i32.
    fn reserve_remote_start(&self, request: RequestId) -> StorageFuture<'_, i32>;
    /// Persists only validated native status and transaction identity, never statusInfo/free text.
    fn record_remote_response(
        &self,
        request: RequestId,
        status: String,
        transaction: Option<String>,
    ) -> StorageFuture<'_, ()>;
    fn remote_control_evidence(
        &self,
        request: RequestId,
    ) -> StorageFuture<'_, Option<RemoteControlEvidence>>;
}
