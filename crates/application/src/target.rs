use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use uob_contracts::{
    CommandResult, ContractVersion, DataPointDescriptor, DataPointValue, EventEnvelope,
    ExternalCommand, Operation, PointId, RequestId, ResourceCapabilities, ResourceRef,
    StationSnapshot, TargetInstanceId, TargetKind, TraceRecord, UtcTimestamp,
};

use crate::{
    DeliveryId, Page, RetainedEventCursor, RetainedEventQuery, SnapshotCursor, SnapshotQuery,
};

/// Future returned by application-owned target ports.
pub type TargetPortFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TargetPortError>> + Send + 'a>>;

/// One boxed, supervised target session.
///
/// Individual messages are received through [`TargetDeliveryReceiver`] and do not require
/// another boxed task.
pub type TargetTask = Pin<Box<dyn Future<Output = Result<(), TargetError>> + Send + 'static>>;

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

/// Secret material is referenced by name or location and is not embedded in configuration.
#[derive(Clone, Eq, PartialEq)]
pub struct CredentialReference(String);

impl CredentialReference {
    /// Creates a non-empty credential reference.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationErrorCode::InvalidField`] for an empty reference.
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigurationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ConfigurationError::field(
                ConfigurationErrorCode::InvalidField,
                "credential_reference",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the reference for a credential provider to resolve at runtime.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialReference([redacted])")
    }
}

/// Typed configuration value accepted at the application-owned boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationValue {
    /// Non-secret text.
    Text(String),
    /// Signed integer.
    Integer(i64),
    /// Boolean flag.
    Boolean(bool),
    /// Reference resolved by the adapter only when the target starts.
    CredentialReference(CredentialReference),
}

/// Primitive kind declared by a target configuration schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationFieldKind {
    /// Non-secret text.
    Text,
    /// Signed integer.
    Integer,
    /// Boolean flag.
    Boolean,
    /// A credential reference, never credential contents.
    CredentialReference,
}

/// One safe field declaration in a target configuration schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationField {
    /// Stable field name.
    pub name: String,
    /// Accepted value kind.
    pub kind: ConfigurationFieldKind,
    /// Whether validation requires the field.
    pub required: bool,
}

/// Safe schema shown by management clients before any target is constructed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigurationSchema {
    /// Complete declared field list.
    pub fields: Vec<ConfigurationField>,
}

/// Unvalidated settings for one configured target instance.
///
/// The type intentionally has no `Debug` implementation, avoiding accidental diagnostic output
/// of non-secret settings while they are being validated.
#[derive(Clone, Eq, PartialEq)]
pub struct TargetConfiguration {
    /// Stable configured instance.
    pub target_instance_id: TargetInstanceId,
    /// Immutable revision to which pending deliveries remain bound.
    pub revision: u64,
    settings: BTreeMap<String, ConfigurationValue>,
}

impl TargetConfiguration {
    /// Creates an unvalidated target configuration.
    #[must_use]
    pub const fn new(target_instance_id: TargetInstanceId, revision: u64) -> Self {
        Self {
            target_instance_id,
            revision,
            settings: BTreeMap::new(),
        }
    }

    /// Adds or replaces one typed setting.
    #[must_use]
    pub fn with_setting(mut self, name: impl Into<String>, value: ConfigurationValue) -> Self {
        self.settings.insert(name.into(), value);
        self
    }

    /// Reads one setting during factory validation.
    #[must_use]
    pub fn setting(&self, name: &str) -> Option<&ConfigurationValue> {
        self.settings.get(name)
    }

    /// Iterates over settings without exposing a driver-specific configuration type.
    pub fn settings(&self) -> impl Iterator<Item = (&str, &ConfigurationValue)> {
        self.settings
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}

/// Configuration proven acceptable by its matching factory.
///
/// Only a factory should construct this value, after validating every declared field. Creating it
/// performs no connection or credential lookup.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedTargetConfiguration(TargetConfiguration);

impl ValidatedTargetConfiguration {
    /// Marks a fully checked configuration as validated.
    #[must_use]
    pub const fn new(configuration: TargetConfiguration) -> Self {
        Self(configuration)
    }

    /// Returns the validated configuration.
    #[must_use]
    pub const fn configuration(&self) -> &TargetConfiguration {
        &self.0
    }

    /// Consumes the proof wrapper.
    #[must_use]
    pub fn into_configuration(self) -> TargetConfiguration {
        self.0
    }
}

/// Stable configuration-validation failure classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationErrorCode {
    /// A required field is absent.
    MissingField,
    /// A supplied field has the wrong type or violates a declared bound.
    InvalidField,
    /// A field is not declared by the schema.
    UnknownField,
    /// The requested configuration mode is not implemented by this target.
    Unsupported,
}

/// Sanitized configuration error that never retains a rejected value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationError {
    code: ConfigurationErrorCode,
    field: Option<String>,
}

impl ConfigurationError {
    /// Creates an error not tied to one field.
    #[must_use]
    pub const fn new(code: ConfigurationErrorCode) -> Self {
        Self { code, field: None }
    }

    /// Creates an error naming only the schema field, never its rejected value.
    #[must_use]
    pub fn field(code: ConfigurationErrorCode, field: impl Into<String>) -> Self {
        Self {
            code,
            field: Some(field.into()),
        }
    }

    /// Returns the stable error category.
    #[must_use]
    pub const fn code(&self) -> ConfigurationErrorCode {
        self.code
    }

    /// Returns the schema field involved, when applicable.
    #[must_use]
    pub fn field_name(&self) -> Option<&str> {
        self.field.as_deref()
    }
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(formatter, "target configuration {:?} at {field}", self.code),
            None => write!(formatter, "target configuration {:?}", self.code),
        }
    }
}

impl Error for ConfigurationError {}

/// Canonical outbound message classes advertised by a target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetMessageClass {
    /// Current canonical station state.
    StationSnapshot,
    /// Durable canonical domain event.
    DomainEvent,
    /// Canonical command lifecycle result.
    CommandResult,
    /// Explicitly optional, redacted diagnostics.
    Diagnostic,
}

/// Delivery guarantees a target can represent to its peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliverySemantic {
    /// Work is exposed locally, without asserting that a peer consumed it.
    LocalExposure,
    /// A named peer can acknowledge a defined delivery scope.
    NamedPeerAcknowledgement,
    /// The protocol can only report an uncertain handoff outcome.
    UncertainHandoff,
}

/// Stable name of an optional target capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetCapability(pub String);

/// Static payload and buffering limits advertised for one target instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetLimits {
    /// Largest canonical message accepted by the target mapping.
    pub maximum_message_bytes: usize,
    /// Largest number of deliveries the adapter may process concurrently.
    pub maximum_in_flight_deliveries: usize,
    /// Largest number of target-originated commands allowed concurrently.
    pub maximum_in_flight_commands: usize,
}

/// Complete supported surface of one configured target instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetDescriptor {
    /// Stable registered adapter kind.
    pub kind: TargetKind,
    /// Stable configured instance identity.
    pub instance_id: TargetInstanceId,
    /// Canonical contract version understood by the adapter.
    pub contract_version: ContractVersion,
    /// Outbound canonical message classes the target represents.
    pub outbound_message_classes: Vec<TargetMessageClass>,
    /// Target-originated operations the adapter can map and authenticate.
    pub inbound_operations: Vec<Operation>,
    /// Explicit payload and concurrency bounds.
    pub limits: TargetLimits,
    /// Delivery facts this target can report without conflating their meaning.
    pub delivery_semantics: Vec<DeliverySemantic>,
    /// Optional capabilities that unrelated targets need not implement.
    pub optional_capabilities: Vec<TargetCapability>,
}

/// Canonical target-neutral message shared immutably by all delivery attempts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetMessage<E> {
    /// Current station state.
    StationSnapshot(StationSnapshot),
    /// Durable domain event with its statically typed payload.
    DomainEvent(EventEnvelope<E>),
    /// Command lifecycle result.
    CommandResult(CommandResult),
    /// Explicitly optional redacted diagnostic record.
    Diagnostic(TraceRecord),
}

/// Host policy for retaining or replacing pending target work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetDeliveryClass {
    /// Durable event or result that remains pending until final policy classification.
    Durable,
    /// Replaceable latest-state update for the same station ordering key.
    ReplaceableLatestState,
}

/// One bounded unit of host-owned outbound work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetDelivery<E> {
    /// Stable delivery identity used by reports and durable retry scheduling.
    pub delivery_id: DeliveryId,
    /// Configured target that owns this work.
    pub target_instance_id: TargetInstanceId,
    /// Immutable target configuration revision that created this work.
    pub target_configuration_revision: u64,
    /// Canonical station/resource ordering key.
    pub station_ordering_key: ResourceRef,
    /// Time after which host policy classifies unfinished delivery.
    pub deadline: UtcTimestamp,
    /// Whether host persistence retains or replaces this work.
    pub class: TargetDeliveryClass,
    /// Shared immutable canonical payload.
    pub message: Arc<TargetMessage<E>>,
}

/// Named scope of a peer acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcknowledgementScope(pub String);

/// Exact outcome of one delivery attempt; intentionally not reducible to a success boolean.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    /// Data was exposed on a named local surface; no remote consumption is asserted.
    LocallyExposed {
        /// Local API, point model, or other named surface.
        surface: String,
    },
    /// A named peer acknowledged one explicitly named scope.
    Acknowledged {
        /// Authenticated or configured peer identity.
        peer: String,
        /// Meaning of the acknowledgement, such as broker receipt or application processing.
        scope: AcknowledgementScope,
    },
    /// The attempt failed before an uncertain handoff and host policy may retry it.
    RetryableFailure {
        /// Stable sanitized reason code.
        reason: String,
    },
    /// The target determined that retrying cannot succeed without configuration or code changes.
    PermanentFailure {
        /// Stable sanitized reason code.
        reason: String,
    },
    /// A handoff may have happened and blind retry could duplicate an external action.
    Uncertain {
        /// Stable sanitized reason code.
        reason: String,
    },
}

/// Critical report returned to host-owned durable delivery policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryReport {
    /// Delivery whose attempt completed or became uncertain.
    pub delivery_id: DeliveryId,
    /// Exact, non-boolean outcome.
    pub outcome: DeliveryOutcome,
    /// UTC instant at which the target established this outcome.
    pub reported_at: UtcTimestamp,
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
mod tests {
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Wake, Waker},
    };

    use time::OffsetDateTime;
    use uob_contracts::{TargetInstanceId, UtcTimestamp};

    use super::*;

    struct EmptyDeliveries;

    impl TargetDeliveryReceiver<()> for EmptyDeliveries {
        fn poll_receive(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<TargetDelivery<()>>> {
            Poll::Ready(None)
        }

        fn capacity(&self) -> usize {
            1
        }

        fn backlog(&self) -> usize {
            0
        }
    }

    struct NoQueries;

    impl TargetQueryPort<()> for NoQueries {
        fn query(&self, _query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<()>> {
            Box::pin(async {
                Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "test.no_queries",
                ))
            })
        }
    }

    impl TargetCommandPort<()> for NoQueries {
        fn submit(&self, _command: ExternalCommand<()>) -> TargetPortFuture<'_, CommandResult> {
            Box::pin(async {
                Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "test.no_commands",
                ))
            })
        }
    }

    impl TargetReportPort for NoQueries {
        fn report(&self, _report: DeliveryReport) -> TargetPortFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    impl TargetDiagnosticPort for NoQueries {
        fn try_emit(&self, _diagnostic: TargetDiagnostic) -> Result<(), DiagnosticDrop> {
            Err(DiagnosticDrop::Disabled)
        }
    }

    struct ImmediateShutdown;

    impl TargetShutdown for ImmediateShutdown {
        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
            Poll::Ready(())
        }
    }

    struct FakeTarget {
        runs: Arc<AtomicUsize>,
        instance_id: TargetInstanceId,
    }

    impl BridgeTarget<(), ()> for FakeTarget {
        fn descriptor(&self) -> TargetDescriptor {
            TargetDescriptor {
                kind: TargetKind::new("test.memory").expect("target kind"),
                instance_id: self.instance_id.clone(),
                contract_version: ContractVersion::V1_INITIAL,
                outbound_message_classes: vec![TargetMessageClass::StationSnapshot],
                inbound_operations: Vec::new(),
                limits: TargetLimits {
                    maximum_message_bytes: 1024,
                    maximum_in_flight_deliveries: 1,
                    maximum_in_flight_commands: 0,
                },
                delivery_semantics: vec![DeliverySemantic::LocalExposure],
                optional_capabilities: Vec::new(),
            }
        }

        fn run(self: Box<Self>, _context: TargetContext<(), ()>) -> TargetTask {
            Box::pin(async move {
                self.runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    struct FakeFactory {
        runtime_connections: Arc<AtomicUsize>,
        runs: Arc<AtomicUsize>,
    }

    impl BridgeTargetFactory<(), ()> for FakeFactory {
        fn kind(&self) -> &'static str {
            "test.memory"
        }

        fn configuration_schema(&self) -> ConfigurationSchema {
            ConfigurationSchema {
                fields: vec![ConfigurationField {
                    name: "credentials".to_owned(),
                    kind: ConfigurationFieldKind::CredentialReference,
                    required: true,
                }],
            }
        }

        fn validate(
            &self,
            configuration: &TargetConfiguration,
        ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
            match configuration.setting("credentials") {
                Some(ConfigurationValue::CredentialReference(_)) => {
                    Ok(ValidatedTargetConfiguration::new(configuration.clone()))
                }
                Some(_) => Err(ConfigurationError::field(
                    ConfigurationErrorCode::InvalidField,
                    "credentials",
                )),
                None => Err(ConfigurationError::field(
                    ConfigurationErrorCode::MissingField,
                    "credentials",
                )),
            }
        }

        fn create(
            &self,
            configuration: ValidatedTargetConfiguration,
        ) -> Result<Box<dyn BridgeTarget<(), ()>>, ConfigurationError> {
            Ok(Box::new(FakeTarget {
                runs: Arc::clone(&self.runs),
                instance_id: configuration.configuration().target_instance_id.clone(),
            }))
        }
    }

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    fn poll_ready(task: &mut TargetTask) -> Poll<Result<(), TargetError>> {
        let waker = Waker::from(Arc::new(NoopWake));
        let mut context = Context::from_waker(&waker);
        task.as_mut().poll(&mut context)
    }

    fn target_context() -> TargetContext<(), ()> {
        let ports = Arc::new(NoQueries);
        TargetContext {
            deliveries: Box::pin(EmptyDeliveries),
            queries: ports.clone(),
            commands: ports.clone(),
            critical_reports: ports.clone(),
            diagnostics: ports,
            limits: TargetRuntimeLimits {
                maximum_in_flight_deliveries: 1,
                maximum_in_flight_commands: 1,
                maximum_command_bytes: 1024,
            },
            shutdown: Box::pin(ImmediateShutdown),
            shutdown_deadline: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
        }
    }

    #[test]
    fn fake_target_is_validated_constructed_and_run_as_a_trait_object() {
        let runtime_connections = Arc::new(AtomicUsize::new(0));
        let runs = Arc::new(AtomicUsize::new(0));
        let factory: Box<dyn BridgeTargetFactory<(), ()>> = Box::new(FakeFactory {
            runtime_connections: Arc::clone(&runtime_connections),
            runs: Arc::clone(&runs),
        });
        let configuration = TargetConfiguration::new(
            TargetInstanceId::new("target-main").expect("target instance"),
            7,
        )
        .with_setting(
            "credentials",
            ConfigurationValue::CredentialReference(
                CredentialReference::new("vault://targets/main").expect("credential reference"),
            ),
        );

        let validated = factory
            .validate(&configuration)
            .expect("valid configuration");
        assert_eq!(runtime_connections.load(Ordering::SeqCst), 0);
        let target: Box<dyn BridgeTarget<(), ()>> =
            factory.create(validated).expect("constructed target");
        assert_eq!(target.descriptor().instance_id.as_str(), "target-main");

        let mut task = target.run(target_context());
        assert_eq!(poll_ready(&mut task), Poll::Ready(Ok(())));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(runtime_connections.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn validation_schema_and_errors_never_retain_credential_contents() {
        let factory = FakeFactory {
            runtime_connections: Arc::new(AtomicUsize::new(0)),
            runs: Arc::new(AtomicUsize::new(0)),
        };
        let credential = "do-not-print-this-secret";
        let invalid = TargetConfiguration::new(
            TargetInstanceId::new("target-main").expect("target instance"),
            1,
        )
        .with_setting(
            "credentials",
            ConfigurationValue::Text(credential.to_owned()),
        );

        let error = match factory.validate(&invalid) {
            Ok(_) => panic!("wrong credential type must fail validation"),
            Err(error) => error,
        };
        let diagnostic = format!("{:?} {error:?} {error}", factory.configuration_schema());

        assert!(!diagnostic.contains(credential));
        assert_eq!(error.code(), ConfigurationErrorCode::InvalidField);
        assert_eq!(factory.runtime_connections.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn delivery_outcomes_preserve_distinct_acknowledgement_meanings() {
        let local = DeliveryOutcome::LocallyExposed {
            surface: "scada.point_model".to_owned(),
        };
        let acknowledged = DeliveryOutcome::Acknowledged {
            peer: "broker-primary".to_owned(),
            scope: AcknowledgementScope("peer.packet_received".to_owned()),
        };
        let retryable = DeliveryOutcome::RetryableFailure {
            reason: "connection_unavailable".to_owned(),
        };
        let permanent = DeliveryOutcome::PermanentFailure {
            reason: "mapping_unsupported".to_owned(),
        };
        let uncertain = DeliveryOutcome::Uncertain {
            reason: "peer_timeout_after_send".to_owned(),
        };

        assert!(matches!(local, DeliveryOutcome::LocallyExposed { .. }));
        assert!(matches!(acknowledged, DeliveryOutcome::Acknowledged { .. }));
        assert!(matches!(
            retryable,
            DeliveryOutcome::RetryableFailure { .. }
        ));
        assert!(matches!(
            permanent,
            DeliveryOutcome::PermanentFailure { .. }
        ));
        assert!(matches!(uncertain, DeliveryOutcome::Uncertain { .. }));
        assert_ne!(local, acknowledged);
        assert_ne!(retryable, permanent);
        assert_ne!(permanent, uncertain);
    }
}
