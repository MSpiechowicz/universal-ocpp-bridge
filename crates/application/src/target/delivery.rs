use std::sync::Arc;

use uob_contracts::{
    CommandResult, ContractVersion, EventEnvelope, Operation, ResourceRef, StationSnapshot,
    TargetInstanceId, TargetKind, TraceRecord, UtcTimestamp,
};

use crate::DeliveryId;

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
