//! Durable backlog facts and audited archive/discard of undrained target deliveries.
//!
//! A disposition is authorized while the old destination may still be running and executed only
//! by the next service start, before any target session can read the outbox. Execution never
//! transfers ownership: archived payloads are retained without dispatch and discarded payloads are
//! removed, both under the audit event recorded at authorization.

use uob_contracts::{PrincipalId, TargetInstanceId, UtcTimestamp};

use crate::StorageFuture;

/// Maximum distinct destinations a backlog summary may report; more fails closed.
pub const MAX_TARGET_BACKLOG_DESTINATIONS: usize = 256;

/// Maximum unsettled dispositions retained at once; more authorizations are refused.
pub const MAX_PENDING_TARGET_DISPOSITIONS: usize = 64;

/// Exact owner of durable target work: one target instance at one configuration revision.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TargetDeliveryDestination {
    /// Stable configured target instance.
    pub target_instance_id: TargetInstanceId,
    /// Configuration revision stamped on the work when it was admitted.
    pub configuration_revision: u64,
}

/// Pending outbox work grouped by its exact destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetBacklogFact {
    /// Instance and revision that must receive or dispose of this work.
    pub destination: TargetDeliveryDestination,
    /// Critical deliveries still awaiting a terminal outcome.
    pub pending_critical_deliveries: u64,
    /// All pending deliveries, including replaceable best-effort telemetry.
    pub pending_deliveries: u64,
}

/// Terminal handling an operator authorized for one old destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryDispositionAction {
    /// Retain payloads in the operator-visible archive without dispatching them.
    Archive,
    /// Permanently remove payloads; requires destructive-operation authorization.
    Discard,
}

/// Lifecycle of one audited disposition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryDispositionState {
    /// Authorized and waiting for the next service start.
    Authorized,
    /// Executed at a start that no longer selected the destination.
    Executed,
    /// Cancelled because the next start still selected the destination.
    Superseded,
}

/// Authenticated request to authorize one disposition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryDispositionRequest {
    /// Exact old destination covered by the audit event.
    pub destination: TargetDeliveryDestination,
    /// Archive or discard.
    pub action: DeliveryDispositionAction,
    /// Authenticated management principal recorded as the actor.
    pub principal_id: PrincipalId,
    /// Trusted authorization time.
    pub authorized_at: UtcTimestamp,
}

/// Durable audit record of one disposition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryDispositionRecord {
    /// Stable audit event identity; the proof accepted by target-change validation.
    pub audit_event_id: String,
    /// Exact old destination covered by the audit event.
    pub destination: TargetDeliveryDestination,
    /// Archive or discard.
    pub action: DeliveryDispositionAction,
    /// Authenticated management principal recorded as the actor.
    pub principal_id: PrincipalId,
    /// Trusted authorization time.
    pub authorized_at: UtcTimestamp,
    /// Current lifecycle state.
    pub state: DeliveryDispositionState,
    /// Start-up time at which the record was executed or superseded.
    pub settled_at: Option<UtcTimestamp>,
    /// Critical deliveries archived or discarded on execution.
    pub critical_deliveries: Option<u64>,
    /// All deliveries archived or discarded on execution.
    pub deliveries: Option<u64>,
}

/// Durable owner of the target outbox backlog and its audited dispositions.
pub trait TargetDispositionStore: Send + Sync {
    /// Summarizes pending outbox work per exact destination.
    ///
    /// Fails closed instead of truncating when more than
    /// [`MAX_TARGET_BACKLOG_DESTINATIONS`] destinations have pending work.
    fn target_delivery_backlog(&self) -> StorageFuture<'_, Vec<TargetBacklogFact>>;

    /// Returns authorized dispositions that the next start has not settled yet.
    fn pending_target_dispositions(&self) -> StorageFuture<'_, Vec<DeliveryDispositionRecord>>;

    /// Records an audited authorization for a destination with pending critical work.
    ///
    /// Rejects a destination without pending critical deliveries, a second unsettled
    /// authorization for the same destination, or more than
    /// [`MAX_PENDING_TARGET_DISPOSITIONS`] unsettled authorizations.
    fn authorize_target_disposition(
        &self,
        request: DeliveryDispositionRequest,
    ) -> StorageFuture<'_, DeliveryDispositionRecord>;

    /// Settles every authorized disposition before a target session starts.
    ///
    /// Authorizations for any destination other than `selected` are executed atomically under
    /// their audit event; an authorization for `selected` itself is superseded because that
    /// destination keeps receiving work. Returns the settled records.
    fn settle_target_dispositions(
        &self,
        selected: Option<TargetDeliveryDestination>,
        settled_at: UtcTimestamp,
    ) -> StorageFuture<'_, Vec<DeliveryDispositionRecord>>;
}
