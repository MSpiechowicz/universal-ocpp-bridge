use std::{
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use uob_contracts::{
    CommandResult, DataPointDescriptor, DataPointValue, EventEnvelope, ExternalCommand, PointId,
    RequestId, ResourceCapabilities, ResourceRef, StationSnapshot, TraceRecord, UtcTimestamp,
};

use crate::{Page, RetainedEventCursor, RetainedEventQuery, SnapshotCursor, SnapshotQuery};

mod configuration;
mod delivery;

pub use configuration::{
    ConfigurationError, ConfigurationErrorCode, ConfigurationField, ConfigurationFieldKind,
    ConfigurationSchema, ConfigurationValue, CredentialReference, TargetConfiguration,
    ValidatedTargetConfiguration,
};
pub use delivery::{
    AcknowledgementScope, DeliveryOutcome, DeliveryReport, DeliverySemantic, TargetCapability,
    TargetDelivery, TargetDeliveryClass, TargetDescriptor, TargetLimits, TargetMessage,
    TargetMessageClass,
};

/// Future returned by application-owned target ports.
pub type TargetPortFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TargetPortError>> + Send + 'a>>;

/// One boxed, supervised target session.
///
/// Individual messages are received through [`TargetDeliveryReceiver`] and do not require
/// another boxed task.
pub type TargetTask = Pin<Box<dyn Future<Output = Result<(), TargetError>> + Send + 'static>>;

/// Boxed bounded subscription to retained durable events.
pub type TargetRetainedEventStream<E> = Pin<Box<dyn TargetSubscription<E>>>;

/// Object-safe lifecycle implemented by every bidirectional target adapter.
///
/// `E` is the application-owned domain-event payload and `P` is the statically typed payload
/// accepted by privileged commands. They preserve typed canonical data while still allowing
/// `dyn BridgeTarget<E, P>` after the application composition root selects those payloads.
pub trait BridgeTarget<E, P>: Send + 'static {
    /// Describes this configured target and its exact supported surface.
    fn descriptor(&self) -> TargetDescriptor;

    /// Starts the one long-lived target task.
    fn run(self: Box<Self>, context: TargetContext<E, P>) -> TargetTask;
}

/// Object-safe, network-free validation and construction boundary for one target kind.
pub trait BridgeTargetFactory<E, P>: Send + Sync {
    /// Stable registered target kind.
    fn kind(&self) -> &'static str;

    /// Safe configuration shape. Credential values are never part of this schema.
    fn configuration_schema(&self) -> ConfigurationSchema;

    /// Validates settings without opening connections or doing other runtime work.
    ///
    /// # Errors
    ///
    /// Returns a sanitized [`ConfigurationError`] when a required setting is missing, invalid,
    /// unknown, or unsupported.
    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError>;

    /// Constructs an inactive target from settings accepted by [`Self::validate`].
    ///
    /// # Errors
    ///
    /// Returns a sanitized [`ConfigurationError`] if the validated settings cannot construct the
    /// advertised target kind. Construction must not connect to the network.
    fn create(
        &self,
        configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<E, P>>, ConfigurationError>;
}

/// Runtime states visible to supervision and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetHealthState {
    /// Target construction completed and its session is starting.
    Starting,
    /// Target can currently perform its advertised work.
    Ready,
    /// Some supported work is impaired while the session remains useful.
    Degraded,
    /// The adapter is recovering its protocol connection.
    Reconnecting,
    /// The supervised session has stopped.
    Stopped,
}

/// Sanitized, bounded health report for one target session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetHealth {
    /// Current lifecycle state.
    pub state: TargetHealthState,
    /// Work currently waiting in the host-provided bounded receiver.
    pub delivery_backlog: usize,
    /// Current in-flight delivery attempts.
    pub in_flight_deliveries: usize,
    /// Current target-owned protocol connections.
    pub active_connections: usize,
    /// Stable sanitized reason code for degraded/reconnecting/stopped states.
    pub reason: Option<String>,
}

/// Stable target session failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetErrorCode {
    /// Supplied runtime context is inconsistent with the descriptor.
    InvalidContext,
    /// Target mapping or protocol configuration cannot be used.
    InvalidConfiguration,
    /// Bounded capacity was exhausted.
    CapacityExhausted,
    /// A protocol connection is unavailable.
    ConnectionUnavailable,
    /// Canonical or mapped data failed integrity checks.
    InvalidData,
    /// Graceful shutdown exceeded its deadline.
    ShutdownDeadlineExceeded,
}

/// Whether host supervision may restart after a target error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorRetryClassification {
    /// Restart can be attempted under bounded backoff policy.
    Retryable,
    /// Restart requires operator or configuration intervention.
    Permanent,
    /// Side effects are uncertain and must be reconciled before retry.
    Uncertain,
}

/// Structured sanitized failure returned by a target session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetError {
    code: TargetErrorCode,
    retry: ErrorRetryClassification,
    context: String,
}

impl TargetError {
    /// Creates a structured error from a stable, pre-sanitized context code.
    #[must_use]
    pub fn new(
        code: TargetErrorCode,
        retry: ErrorRetryClassification,
        context: impl Into<String>,
    ) -> Self {
        Self {
            code,
            retry,
            context: context.into(),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(&self) -> TargetErrorCode {
        self.code
    }

    /// Returns restart/reconciliation classification.
    #[must_use]
    pub const fn retry_classification(&self) -> ErrorRetryClassification {
        self.retry
    }

    /// Returns bounded sanitized context, never a raw protocol payload or source error.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl fmt::Display for TargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "target {:?} ({:?}): {}",
            self.code, self.retry, self.context
        )
    }
}

impl Error for TargetError {}

/// Stable failures returned by scoped application ports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetPortErrorCode {
    /// The target's scope does not permit the request.
    Unauthorized,
    /// The target does not advertise this operation or query.
    Unsupported,
    /// The request expired before admission.
    Expired,
    /// A bounded application queue cannot currently accept work.
    Busy,
    /// Authoritative state is temporarily unavailable.
    Unavailable,
    /// The request or cursor is invalid.
    InvalidRequest,
    /// A retained-event cursor is outside the available retention window.
    CursorExpired,
}

/// Sanitized scoped-port error with no database, socket, or driver source chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetPortError {
    code: TargetPortErrorCode,
    context: String,
}

impl TargetPortError {
    /// Creates an error from a stable, sanitized context code.
    #[must_use]
    pub fn new(code: TargetPortErrorCode, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    /// Returns the stable machine-readable category.
    #[must_use]
    pub const fn code(&self) -> TargetPortErrorCode {
        self.code
    }

    /// Returns sanitized application context.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }
}

impl fmt::Display for TargetPortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "target port {:?}: {}", self.code, self.context)
    }
}

impl Error for TargetPortError {}

/// Scoped canonical query accepted from a target adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetQuery {
    /// Read one current station snapshot.
    StationSnapshot(ResourceRef),
    /// Read a bounded page of current station snapshots.
    StationSnapshots(SnapshotQuery),
    /// Read one canonical point descriptor.
    DataPointDescriptor {
        /// Resource owning the point.
        resource: ResourceRef,
        /// Stable point identity.
        point_id: PointId,
    },
    /// Read one current canonical point value.
    DataPointValue {
        /// Resource owning the point.
        resource: ResourceRef,
        /// Stable point identity.
        point_id: PointId,
    },
    /// Read explicitly advertised resource capabilities.
    Capabilities(ResourceRef),
    /// Read the latest canonical result for one command request.
    CommandResult(RequestId),
    /// Read a bounded page of retained events.
    RetainedEvents(RetainedEventQuery),
}

/// Typed result returned by a scoped canonical query port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetQueryResult<E> {
    /// One current station snapshot, when known and authorized.
    StationSnapshot(Option<StationSnapshot>),
    /// Bounded current snapshot page.
    StationSnapshots(Page<StationSnapshot, SnapshotCursor>),
    /// One point descriptor, when known and authorized.
    DataPointDescriptor(Option<DataPointDescriptor>),
    /// One current point value, when known and authorized.
    DataPointValue(Option<DataPointValue>),
    /// Resource capabilities, when known and authorized.
    Capabilities(Option<ResourceCapabilities>),
    /// Latest command result, when known and authorized.
    CommandResult(Option<CommandResult>),
    /// Bounded retained-event page.
    RetainedEvents(Page<EventEnvelope<E>, RetainedEventCursor>),
}

/// Host-owned query surface already restricted to the configured target's authorization scope.
pub trait TargetQueryPort<E>: Send + Sync {
    /// Executes one target-neutral canonical query.
    fn query(&self, query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<E>>;

    /// Opens a bounded stream of retained durable events from an optional cursor.
    fn subscribe_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<E>>;
}

/// Poll-based retained-event subscription supplied to a target adapter.
///
/// This surface carries only durable [`EventEnvelope`] records. Best-effort telemetry uses a
/// separate diagnostic/delivery surface and cannot be mistaken for resumable history.
pub trait TargetSubscription<E>: Send {
    /// Polls the next retained event. Stream failures remain structured and sanitized.
    fn poll_event(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<EventEnvelope<E>, TargetPortError>>>;

    /// Maximum number of events the subscription may buffer.
    fn capacity(&self) -> usize;

    /// Current buffered event count used for bounded diagnostics.
    fn backlog(&self) -> usize;
}

/// Host-owned command ingress that reapplies authorization, capability, safety and idempotency.
pub trait TargetCommandPort<P>: Send + Sync {
    /// Submits an authenticated target-originated command through the ordinary application path.
    fn submit(&self, command: ExternalCommand<P>) -> TargetPortFuture<'_, CommandResult>;
}

/// Poll-based bounded delivery receiver; it exposes no queue implementation or storage handle.
pub trait TargetDeliveryReceiver<E>: Send {
    /// Polls the next host-owned delivery without allocating a boxed future per message.
    fn poll_receive(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<TargetDelivery<E>>>;

    /// Maximum number of queued items allowed by this receiver.
    fn capacity(&self) -> usize;

    /// Current queued item count used for bounded health reporting.
    fn backlog(&self) -> usize;
}

/// Reliable critical-report channel consumed by durable host delivery policy.
pub trait TargetReportPort: Send + Sync {
    /// Reports an exact delivery outcome, applying bounded backpressure when necessary.
    fn report(&self, report: DeliveryReport) -> TargetPortFuture<'_, ()>;
}

/// Low-priority target diagnostics sent separately from critical delivery reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetDiagnostic {
    /// Structured target lifecycle and resource state.
    Health(TargetHealth),
    /// Already-redacted trace record.
    Trace(TraceRecord),
}

/// Explicit reason a best-effort diagnostic was not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticDrop {
    /// The bounded diagnostic channel is full.
    Full,
    /// Diagnostics are disabled for this target.
    Disabled,
    /// The diagnostic channel is shutting down.
    Closed,
}

/// Best-effort diagnostic channel that cannot block critical delivery reports.
pub trait TargetDiagnosticPort: Send + Sync {
    /// Attempts to emit one diagnostic without waiting for capacity.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticDrop`] when the best-effort channel is full, disabled, or closed.
    fn try_emit(&self, diagnostic: TargetDiagnostic) -> Result<(), DiagnosticDrop>;
}

/// Poll-based shutdown signal independent of any async runtime implementation.
pub trait TargetShutdown: Send {
    /// Resolves when supervision requests graceful shutdown.
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()>;
}

/// Host-enforced resource bounds for one running target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetRuntimeLimits {
    /// Maximum number of concurrent delivery attempts.
    pub maximum_in_flight_deliveries: usize,
    /// Maximum number of concurrent target-originated commands.
    pub maximum_in_flight_commands: usize,
    /// Maximum canonical payload size accepted from this target.
    pub maximum_command_bytes: usize,
}

/// Complete host-owned capability set supplied to one target session.
///
/// The context deliberately exposes only bounded deliveries and scoped application ports. There
/// is no database connection, charger socket, or concrete queue type.
pub struct TargetContext<E, P> {
    /// Bounded outbound delivery stream.
    pub deliveries: Pin<Box<dyn TargetDeliveryReceiver<E>>>,
    /// Scoped canonical queries.
    pub queries: Arc<dyn TargetQueryPort<E>>,
    /// Authorized canonical command ingress.
    pub commands: Arc<dyn TargetCommandPort<P>>,
    /// Critical delivery reports, separate from diagnostics.
    pub critical_reports: Arc<dyn TargetReportPort>,
    /// Low-priority health and tracing channel.
    pub diagnostics: Arc<dyn TargetDiagnosticPort>,
    /// Host-enforced runtime bounds.
    pub limits: TargetRuntimeLimits,
    /// Graceful shutdown signal.
    pub shutdown: Pin<Box<dyn TargetShutdown>>,
    /// Absolute deadline by which graceful shutdown must finish.
    pub shutdown_deadline: UtcTimestamp,
}

#[cfg(test)]
mod tests;
