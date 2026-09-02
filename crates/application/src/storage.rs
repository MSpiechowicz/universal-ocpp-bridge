use std::{error::Error, fmt, future::Future, pin::Pin};

use uob_contracts::{
    Command, CommandResult, EventEnvelope, EventId, RequestId, ResourceRef, StationSnapshot,
    TargetInstanceId, UtcTimestamp,
};

use crate::{DeliveryOutcome, DeliveryReport};

/// Largest page a caller may request from an operational read port.
pub const MAX_PAGE_SIZE: u16 = 100;

/// Minimum lifetime of a resolved command's idempotency record.
pub const COMMAND_DEDUPLICATION_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Object-safe future returned by operational storage implementations.
pub type StorageFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StorageError>> + Send + 'a>>;

macro_rules! storage_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Creates a non-empty stable identity.
            ///
            /// # Errors
            ///
            /// Returns [`StorageError`] when `value` has no visible characters.
            pub fn new(value: impl Into<String>) -> Result<Self, StorageError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(StorageError::new(
                        StorageErrorCode::InvalidRequest,
                        concat!(stringify!($name), " cannot be empty"),
                    ));
                }
                Ok(Self(value))
            }

            /// Returns the stable representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

storage_id!(
    AuthorizationReference,
    "Opaque reference to locally managed authorization state."
);
storage_id!(
    DeliveryId,
    "Stable identity of one required target delivery."
);
storage_id!(
    CommittedRecordId,
    "Stable identity of one record visible to incremental export."
);
storage_id!(SnapshotCursor, "Opaque cursor for station snapshot pages.");
storage_id!(
    RetainedEventCursor,
    "Opaque cursor for a retained durable event stream."
);
storage_id!(
    CommittedRecordCursor,
    "Opaque cursor for incrementally committed records."
);

/// Validated non-zero bound for one storage read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageLimit(u16);

impl PageLimit {
    /// Creates a page limit between one and [`MAX_PAGE_SIZE`], inclusive.
    ///
    /// # Errors
    ///
    /// Returns [`PageLimitError`] for zero or an oversized request.
    pub const fn new(value: u16) -> Result<Self, PageLimitError> {
        if value == 0 {
            Err(PageLimitError::Zero)
        } else if value > MAX_PAGE_SIZE {
            Err(PageLimitError::TooLarge {
                requested: value,
                maximum: MAX_PAGE_SIZE,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the validated bound.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Invalid bounded-read request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageLimitError {
    /// At least one item must be requested.
    Zero,
    /// The requested page would exceed the application-owned maximum.
    TooLarge {
        /// Invalid requested size.
        requested: u16,
        /// Largest accepted size.
        maximum: u16,
    },
}

impl fmt::Display for PageLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("page limit must be greater than zero"),
            Self::TooLarge { requested, maximum } => {
                write!(
                    formatter,
                    "page limit {requested} exceeds maximum {maximum}"
                )
            }
        }
    }
}

impl Error for PageLimitError {}

/// Whether loss of a record may affect charging recovery or only observability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Record must commit atomically with its owning state change.
    Critical,
    /// Record may be shed according to an explicit bounded telemetry policy.
    BestEffortTelemetry,
}

/// Current state of one locally managed authorization reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationState {
    /// Reference may be considered by local authorization policy.
    Active,
    /// Reference has been explicitly revoked.
    Revoked,
}

/// Versioned local authorization mutation included in an atomic write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationChange {
    /// Opaque authorization reference; never a credential or token value.
    pub reference: AuthorizationReference,
    /// Resource scope to which the reference applies.
    pub resource: ResourceRef,
    /// New authorization state.
    pub state: AuthorizationState,
    /// Monotonic revision used to reject stale changes.
    pub revision: u64,
    /// Trusted observation time of this change.
    pub changed_at: UtcTimestamp,
}

/// Required target delivery stored alongside the state that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDelivery<D> {
    /// Stable delivery identity used for retry and acknowledgement.
    pub delivery_id: DeliveryId,
    /// Durable event whose delivery state this row represents.
    pub event_id: EventId,
    /// Configured target that owns this delivery.
    pub target_instance_id: TargetInstanceId,
    /// Immutable revision of the selected target configuration.
    pub target_configuration_revision: u64,
    /// Canonical resource ordering key.
    pub ordering_key: ResourceRef,
    /// Deadline after which delivery policy must classify the pending work.
    pub deadline: UtcTimestamp,
    /// Whether loss can affect recovery guarantees.
    pub durability: Durability,
    /// Typed, target-neutral payload.
    pub payload: D,
}

/// Bounded query for target work whose persisted retry delay has elapsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDeliveryQuery {
    /// Only work owned by this configured target is returned.
    pub target_instance_id: TargetInstanceId,
    /// Only work created by this immutable configuration revision is returned.
    pub target_configuration_revision: u64,
    /// Trusted host time used to exclude delayed retry work.
    pub ready_at: UtcTimestamp,
    /// Maximum number of deliveries returned.
    pub limit: PageLimit,
}

/// One ready outbox entry together with its durable attempt count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledDelivery<D> {
    /// Stable target-neutral work recovered from the outbox.
    pub delivery: PendingDelivery<D>,
    /// Reports already recorded for this delivery, retained across restarts.
    pub attempt_count: u32,
}

/// Host policy decision persisted atomically with an exact delivery report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryAttemptResolution {
    /// Keep the outbox row and make it eligible again at this instant.
    RetryAt(UtcTimestamp),
    /// Remove the outbox row after preserving the exact final report.
    Final,
}

/// Exact target report and host policy decision for one attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryAttempt {
    /// Exact non-boolean outcome emitted by the target adapter.
    pub report: DeliveryReport,
    /// Whether policy retries or finalizes the pending work.
    pub resolution: DeliveryAttemptResolution,
}

/// Durable audit view of one target delivery attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedDeliveryAttempt {
    /// Stable delivery identity shared by every retry.
    pub delivery_id: DeliveryId,
    /// Exact outcome, without collapsing local exposure into peer acknowledgement.
    pub outcome: DeliveryOutcome,
    /// Target-reported time for the attempt.
    pub reported_at: UtcTimestamp,
    /// Persisted host policy decision.
    pub resolution: DeliveryAttemptResolution,
}

/// Record exposed only after the atomic write that produced it commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecord<R> {
    /// Stable identity retained across at-least-once export attempts.
    pub record_id: CommittedRecordId,
    /// Whether this record is durable or explicitly best effort.
    pub durability: Durability,
    /// Trusted commit observation time.
    pub committed_at: UtcTimestamp,
    /// Typed canonical record; storage adapters do not interpret its wire representation.
    pub record: R,
}

/// One application-defined transaction boundary for authoritative local persistence.
///
/// Implementations must make all populated fields visible together or leave all of them
/// invisible. In particular, a newly admitted command, its journal event, and its required
/// deliveries cannot be observed independently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicStoreWrite<C, E, D, R> {
    /// Latest charging state produced by the operation, when it changed.
    pub station_snapshot: Option<StationSnapshot>,
    /// Local authorization mutations owned by this operation.
    pub authorization_changes: Vec<AuthorizationChange>,
    /// Command to admit with request-ID deduplication, when applicable.
    pub command: Option<Command<C>>,
    /// Latest command result to persist, when applicable.
    pub command_result: Option<CommandResult>,
    /// Durable journal records produced by the operation.
    pub journal_events: Vec<EventEnvelope<E>>,
    /// Required target work produced by the operation.
    pub required_deliveries: Vec<PendingDelivery<D>>,
    /// Records made available to incremental export only after commit.
    pub committed_records: Vec<CommittedRecord<R>>,
}

impl<C, E, D, R> AtomicStoreWrite<C, E, D, R> {
    /// Creates an empty write unit for callers to populate explicitly.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            station_snapshot: None,
            authorization_changes: Vec::new(),
            command: None,
            command_result: None,
            journal_events: Vec::new(),
            required_deliveries: Vec::new(),
            committed_records: Vec::new(),
        }
    }
}

/// Result of request-ID deduplication performed inside the atomic commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandAdmissionOutcome {
    /// This transaction durably admitted a previously unseen request.
    Admitted,
    /// An identical command was already durably admitted.
    Duplicate {
        /// Latest durable result known for the original command, when one has been recorded.
        result: Option<Box<CommandResult>>,
    },
}

/// Visible outcome of one successful atomic write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicWriteOutcome {
    /// Admission outcome when the write included a command.
    pub command: Option<CommandAdmissionOutcome>,
}

/// Generic bounded page with a stream-specific continuation cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T, C> {
    /// Items in stable stream order, never exceeding the requested limit.
    pub items: Vec<T>,
    /// Cursor for the next page, or `None` when the current end was reached.
    pub next_cursor: Option<C>,
}

/// Bounded station snapshot query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotQuery {
    /// Continue after this opaque cursor.
    pub after: Option<SnapshotCursor>,
    /// Maximum number of snapshots returned.
    pub limit: PageLimit,
}

/// Bounded retained-event query scoped to one resource stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedEventQuery {
    /// Resource whose ordered event stream is requested.
    pub resource: ResourceRef,
    /// Continue after this opaque cursor.
    pub after: Option<RetainedEventCursor>,
    /// Maximum number of events returned.
    pub limit: PageLimit,
}

/// Bounded incremental committed-record query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecordQuery {
    /// Continue after this opaque cursor.
    pub after: Option<CommittedRecordCursor>,
    /// Maximum number of records returned.
    pub limit: PageLimit,
    /// Include explicitly best-effort telemetry in addition to critical records.
    pub include_best_effort_telemetry: bool,
}

/// Bounded authoritative state needed to resume application work after restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryQuery {
    /// Maximum number of commands and deliveries returned in this batch.
    pub limit: PageLimit,
}

/// One bounded recovery view of authoritative operational state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryBatch<C, D> {
    /// Latest committed station states.
    pub station_snapshots: Vec<StationSnapshot>,
    /// Latest committed authorization revisions required for local decisions.
    pub authorization: Vec<AuthorizationChange>,
    /// Commands that have not reached a terminal lifecycle state.
    pub active_commands: Vec<Command<C>>,
    /// Latest results for recovered active commands.
    pub command_results: Vec<CommandResult>,
    /// Required deliveries that have not reached a final outcome.
    pub pending_deliveries: Vec<PendingDelivery<D>>,
    /// Whether another bounded recovery batch remains.
    pub has_more: bool,
}

/// Stable, sanitized operational storage error categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageErrorCode {
    /// A caller supplied an invalid bound, cursor, or operation.
    InvalidRequest,
    /// An optimistic revision or existing identity conflicts with this request.
    Conflict,
    /// A supplied cursor is no longer retained and cannot be resumed.
    CursorExpired,
    /// Bounded authoritative capacity is exhausted.
    CapacityExhausted,
    /// The storage worker cannot currently accept more work.
    Busy,
    /// The implementation is temporarily unavailable.
    Unavailable,
    /// Previously committed authoritative state failed integrity validation.
    IntegrityFailure,
}

/// Sanitized failure returned across the application-owned storage boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageError {
    code: StorageErrorCode,
    detail: String,
}

impl StorageError {
    /// Creates an error from a stable code and pre-sanitized operator-facing detail.
    #[must_use]
    pub fn new(code: StorageErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> StorageErrorCode {
        self.code
    }

    /// Returns sanitized detail without an implementation source chain.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.code, self.detail)
    }
}

impl Error for StorageError {}

/// Application-owned authoritative persistence and committed-record read ports.
///
/// The type parameters keep evolving canonical payloads typed while allowing callers to use
/// `dyn OperationalStore<C, E, D, R>`. Methods deliberately expose no driver connection,
/// statement, or physical-layout types.
pub trait OperationalStore<C, E, D, R>: Send + Sync {
    /// Atomically persists one application operation and performs command deduplication.
    /// An identical duplicate returns [`CommandAdmissionOutcome::Duplicate`] without applying
    /// the bundled state, journal, delivery, or committed-record mutations again. Reusing a
    /// request ID for a different command returns [`StorageErrorCode::Conflict`].
    fn write_atomic(
        &self,
        write: AtomicStoreWrite<C, E, D, R>,
    ) -> StorageFuture<'_, AtomicWriteOutcome>;

    /// Reads a bounded page of latest station snapshots.
    fn read_snapshots(
        &self,
        query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>>;

    /// Reads a bounded page from one retained durable event stream.
    fn read_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> StorageFuture<'_, Page<EventEnvelope<E>, RetainedEventCursor>>;

    /// Reads records that became visible through completed atomic writes.
    fn read_committed_records(
        &self,
        query: CommittedRecordQuery,
    ) -> StorageFuture<'_, Page<CommittedRecord<R>, CommittedRecordCursor>>;

    /// Reads a bounded recovery view without granting unrestricted storage access.
    fn recover(&self, query: RecoveryQuery) -> StorageFuture<'_, RecoveryBatch<C, D>>;

    /// Reads one durably admitted command by idempotency identity.
    fn command_by_request_id(&self, request_id: RequestId)
    -> StorageFuture<'_, Option<Command<C>>>;

    /// Removes resolved command identities whose seven-day retention period has elapsed.
    ///
    /// Commands without a terminal result, including transmission-uncertain commands, must be
    /// retained regardless of age. The returned count is the number of removed identities.
    fn prune_command_deduplication(&self, now: UtcTimestamp) -> StorageFuture<'_, u64>;
}

/// Narrow durable outbox surface used by target delivery supervision.
///
/// Keeping this separate from [`OperationalStore`] prevents the target worker from gaining
/// command, authorization, snapshot, journal, or external-export access.
pub trait TargetDeliveryStore<D>: Send + Sync {
    /// Reads ready work in stable insertion order while preserving head-of-line ordering for each
    /// canonical resource key.
    fn read_pending_deliveries(
        &self,
        query: PendingDeliveryQuery,
    ) -> StorageFuture<'_, Vec<ScheduledDelivery<D>>>;

    /// Persists the exact report and its retry/final decision in one transaction.
    fn record_delivery_attempt(&self, attempt: DeliveryAttempt) -> StorageFuture<'_, ()>;

    /// Reads the bounded audit history for one stable delivery identity, newest first.
    fn delivery_attempts(
        &self,
        delivery_id: DeliveryId,
        limit: PageLimit,
    ) -> StorageFuture<'_, Vec<RecordedDeliveryAttempt>>;
}
