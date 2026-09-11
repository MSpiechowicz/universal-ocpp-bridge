//! Trusted service/supervisor boundary; never exposed as an alternate command API.
use crate::StorageFuture;
use std::{sync::Arc, time::Duration};

/// Process-local ownership of one maintenance window. Never persist or accept from a client.
#[derive(Clone, Debug)]
pub struct DrainId(Arc<()>);
impl Default for DrainId {
    fn default() -> Self {
        Self::new()
    }
}
impl DrainId {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(()))
    }
    #[must_use]
    pub fn same_window(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Exact worker revision and durable work inventory, not a reusable activation permit.
#[derive(Clone, Debug)]
pub struct DrainObservation {
    pub window: DrainId,
    pub revision: u64,
    pub active_transactions: u64,
    pub unresolved_commands: u64,
    pub stateful_jobs: u64,
}
impl DrainObservation {
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.active_transactions == 0 && self.unresolved_commands == 0 && self.stateful_jobs == 0
    }
}

/// Durable stateful operations whose physical work must finish before an upgrade.
#[derive(Clone, Copy, Debug)]
pub enum ReleaseJobKind {
    Firmware,
    Certificate,
}

/// One authority shared by every operational-store client in the production process.
/// Implementations serialize observation and sealing with writes. Sealing freezes writes;
/// a process controller must stop the old process before switching artifacts. A seal is
/// process-local and expires with its maintenance window; it is never qualification evidence.
pub trait ReleaseDrainPort: Send + Sync {
    fn begin_drain(&self, window: Duration) -> StorageFuture<'_, DrainId>;
    fn observe_drain(&self, window: DrainId) -> StorageFuture<'_, DrainObservation>;
    fn seal_drain(&self, observation: DrainObservation) -> StorageFuture<'_, ()>;
    /// Must enqueue cancellation when called, even if the returned future is dropped.
    /// A full queue must leave the independently enforced window deadline intact.
    fn cancel_drain(&self, window: DrainId) -> StorageFuture<'_, ()>;
    /// Persist before issuing a stateful job to a peer; only finish after confirmed resolution.
    fn start_release_job(&self, id: String, kind: ReleaseJobKind) -> StorageFuture<'_, ()>;
    fn finish_release_job(&self, id: String) -> StorageFuture<'_, ()>;
}
