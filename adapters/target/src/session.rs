use std::{error::Error, fmt, sync::Arc, time::Duration};

use serde::Serialize;
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinError, JoinHandle},
    time::timeout,
};
use uob_application::{
    AccessPolicy, AdmissionError, BridgeTarget, CommandAdmissionPort, ConfigurationError,
    ErrorRetryClassification, RuntimeResourceBudget, TargetContext, TargetDelivery,
    TargetDiagnostic, TargetDiagnosticPort, TargetError, TargetHealth, TargetHealthState,
    TargetQueryPort, TargetReportPort, TargetRuntimeLimits, WorkClass,
};
use uob_contracts::{ContractVersion, UtcTimestamp};

use crate::{TargetDestination, ValidatedTargetSelection};

mod ports;

use ports::{
    BoundedDeliveryReceiver, QueuedDelivery, ShutdownSignal, bounded_reports, guarded_commands,
};

/// Host-owned ports supplied to one selected target session.
pub struct TargetSessionPorts<E, P> {
    /// Canonical query access already scoped to this target instance.
    pub queries: Arc<dyn TargetQueryPort<E>>,
    /// Common application command admission path.
    pub commands: Arc<dyn CommandAdmissionPort<P>>,
    /// Authenticated principal, permissions, and canonical resources granted to this target.
    pub command_authorization: AccessPolicy,
    /// Durable host delivery policy receiving exact target outcomes.
    pub critical_reports: Arc<dyn TargetReportPort>,
    /// Best-effort diagnostics, independent from critical reports.
    pub diagnostics: Arc<dyn TargetDiagnosticPort>,
}

/// Explicit bounds and shutdown deadline for one target session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetSessionOptions {
    /// Maximum host-owned deliveries waiting for the adapter.
    pub delivery_capacity: usize,
    /// Maximum critical reports concurrently waiting on durable host policy.
    pub critical_report_capacity: usize,
    /// Limits exposed to and enforced around the adapter.
    pub runtime_limits: TargetRuntimeLimits,
    /// Absolute deadline advertised to the target for graceful shutdown.
    pub shutdown_deadline: UtcTimestamp,
}

/// Cloneable, nonblocking ingress for runtime and recovered target work.
pub struct TargetDeliveryIngress<E> {
    sender: mpsc::Sender<QueuedDelivery<E>>,
    destination: TargetDestination,
    budget: Arc<RuntimeResourceBudget>,
}

impl<E> Clone for TargetDeliveryIngress<E> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            destination: self.destination.clone(),
            budget: Arc::clone(&self.budget),
        }
    }
}

impl<E> TargetDeliveryIngress<E> {
    /// Enqueues one already-ordered delivery without waiting for adapter availability.
    ///
    /// The caller supplies the encoded canonical size used by shared process accounting. FIFO
    /// insertion preserves the caller's order, including the order recovered from host storage.
    ///
    /// # Errors
    ///
    /// Rejects another target/revision, exhausted shared capacity, a full queue, or a stopped
    /// target. The original delivery is always returned to the caller on failure.
    pub fn try_deliver(
        &self,
        delivery: TargetDelivery<E>,
        encoded_bytes: usize,
    ) -> Result<(), TargetDeliveryIngressError<E>> {
        if self.destination.target_instance_id != delivery.target_instance_id
            || self.destination.configuration_revision != delivery.target_configuration_revision
        {
            return Err(TargetDeliveryIngressError::DestinationMismatch(Box::new(
                delivery,
            )));
        }
        let reservation = match self
            .budget
            .try_reserve(WorkClass::TargetEgress, encoded_bytes)
        {
            Ok(reservation) => reservation,
            Err(error) => {
                return Err(TargetDeliveryIngressError::Capacity {
                    delivery: Box::new(delivery),
                    error,
                });
            }
        };
        self.sender
            .try_send(QueuedDelivery {
                delivery,
                _reservation: reservation,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(queued) => {
                    TargetDeliveryIngressError::Full(Box::new(queued.delivery))
                }
                mpsc::error::TrySendError::Closed(queued) => {
                    TargetDeliveryIngressError::Closed(Box::new(queued.delivery))
                }
            })
    }

    /// Exact target instance and configuration revision accepted by this ingress.
    #[must_use]
    pub const fn destination(&self) -> &TargetDestination {
        &self.destination
    }
}

/// Delivery admission failure that never loses ownership of the rejected work.
#[derive(Debug)]
pub enum TargetDeliveryIngressError<E> {
    /// Work belongs to another target instance or immutable configuration revision.
    DestinationMismatch(Box<TargetDelivery<E>>),
    /// Shared item or byte capacity is exhausted.
    Capacity {
        /// Original delivery.
        delivery: Box<TargetDelivery<E>>,
        /// Exact shared resource rejection.
        error: AdmissionError,
    },
    /// The bounded target queue is full.
    Full(Box<TargetDelivery<E>>),
    /// The selected target session has stopped.
    Closed(Box<TargetDelivery<E>>),
}

impl<E> fmt::Display for TargetDeliveryIngressError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationMismatch(_) => {
                formatter.write_str("target delivery destination mismatch")
            }
            Self::Capacity { error, .. } => {
                write!(formatter, "target delivery rejected: {error:?}")
            }
            Self::Full(_) => formatter.write_str("target delivery queue is full"),
            Self::Closed(_) => formatter.write_str("target delivery queue is closed"),
        }
    }
}

impl<E: fmt::Debug> Error for TargetDeliveryIngressError<E> {}

/// Owning handle for exactly one selected target task.
pub struct TargetSessionTask {
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<Result<(), TargetError>>>,
}

impl TargetSessionTask {
    /// Waits until the selected target stops without requesting shutdown.
    ///
    /// # Errors
    ///
    /// Returns the target failure, panic, cancellation, or an unavailable supervisor handle.
    pub async fn wait(mut self) -> Result<(), TargetSessionError> {
        let shutdown = self.shutdown.take();
        let Some(join) = self.join.take() else {
            return Err(TargetSessionError::SupervisorUnavailable);
        };
        let result = flatten_join(join.await);
        drop(shutdown);
        result
    }

    /// Requests graceful shutdown and aborts a target that misses the supplied duration.
    ///
    /// # Errors
    ///
    /// Returns the target/join failure or [`TargetSessionError::ShutdownDeadlineExceeded`].
    pub async fn shutdown(mut self, deadline: Duration) -> Result<(), TargetSessionError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(mut join) = self.join.take() else {
            return Err(TargetSessionError::SupervisorUnavailable);
        };
        if let Ok(result) = timeout(deadline, &mut join).await {
            flatten_join(result)
        } else {
            join.abort();
            let _ = join.await;
            Err(TargetSessionError::ShutdownDeadlineExceeded)
        }
    }
}

impl Drop for TargetSessionTask {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

/// Starts only the target factory selected and validated by the registry.
///
/// The returned ingress can be populated from durable recovery before ordinary runtime delivery.
/// Starting another session from the same selection reconstructs the adapter while keeping all
/// pending work in host ownership.
///
/// # Errors
///
/// Rejects invalid bounds, descriptor/selection mismatches, and factory construction failures.
pub fn spawn_target_session<E, P>(
    selection: &ValidatedTargetSelection<E, P>,
    ports: TargetSessionPorts<E, P>,
    budget: Arc<RuntimeResourceBudget>,
    options: TargetSessionOptions,
) -> Result<(TargetDeliveryIngress<E>, TargetSessionTask), TargetSessionError>
where
    E: Send + Sync + 'static,
    P: Serialize + Send + 'static,
{
    validate_options(&budget, options)?;
    let target = selection
        .create()
        .map_err(TargetSessionError::Construction)?;
    let descriptor = target.descriptor();
    validate_descriptor(selection, &descriptor, options.runtime_limits)?;

    let destination = selection.destination();
    let (delivery_sender, delivery_receiver) = mpsc::channel(options.delivery_capacity);
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let commands = guarded_commands(
        ports.commands,
        ports.command_authorization,
        destination.target_instance_id.clone(),
        descriptor.inbound_operations,
        options.runtime_limits,
    );
    let reports = bounded_reports(
        ports.critical_reports,
        Arc::clone(&budget),
        options.critical_report_capacity,
    );
    let context = TargetContext {
        deliveries: Box::pin(BoundedDeliveryReceiver(delivery_receiver)),
        queries: ports.queries,
        commands,
        critical_reports: reports,
        diagnostics: Arc::clone(&ports.diagnostics),
        limits: options.runtime_limits,
        shutdown: Box::pin(ShutdownSignal(shutdown_receiver)),
        shutdown_deadline: options.shutdown_deadline,
    };

    emit_health(
        &ports.diagnostics,
        TargetHealthState::Starting,
        "target.session_starting",
    );
    let diagnostics = ports.diagnostics;
    let join = tokio::spawn(run_target(target, context, diagnostics));
    Ok((
        TargetDeliveryIngress {
            sender: delivery_sender,
            destination,
            budget,
        },
        TargetSessionTask {
            shutdown: Some(shutdown_sender),
            join: Some(join),
        },
    ))
}

async fn run_target<E, P>(
    target: Box<dyn BridgeTarget<E, P>>,
    context: TargetContext<E, P>,
    diagnostics: Arc<dyn TargetDiagnosticPort>,
) -> Result<(), TargetError>
where
    E: 'static,
    P: 'static,
{
    let result = target.run(context).await;
    match &result {
        Ok(()) => emit_health(
            &diagnostics,
            TargetHealthState::Stopped,
            "target.session_stopped",
        ),
        Err(error) => {
            let (state, reason) = match error.retry_classification() {
                ErrorRetryClassification::Retryable => {
                    (TargetHealthState::Reconnecting, "target.session_retryable")
                }
                ErrorRetryClassification::Uncertain => {
                    (TargetHealthState::Degraded, "target.session_uncertain")
                }
                ErrorRetryClassification::Permanent => {
                    (TargetHealthState::Stopped, "target.session_permanent")
                }
            };
            emit_health(&diagnostics, state, reason);
        }
    }
    result
}

fn emit_health(
    diagnostics: &Arc<dyn TargetDiagnosticPort>,
    state: TargetHealthState,
    reason: &str,
) {
    let _ = diagnostics.try_emit(TargetDiagnostic::Health(TargetHealth {
        state,
        delivery_backlog: 0,
        in_flight_deliveries: 0,
        active_connections: 0,
        reason: Some(reason.to_owned()),
    }));
}

fn validate_options(
    budget: &RuntimeResourceBudget,
    options: TargetSessionOptions,
) -> Result<(), TargetSessionError> {
    let queue_limits = &budget.limits().queues;
    if options.delivery_capacity == 0
        || options.delivery_capacity > queue_limits.target_egress
        || options.critical_report_capacity == 0
        || options.critical_report_capacity > queue_limits.critical_reports
        || options.runtime_limits.maximum_in_flight_deliveries == 0
        || options.runtime_limits.maximum_command_bytes == 0
    {
        return Err(TargetSessionError::InvalidLimits);
    }
    Ok(())
}

fn validate_descriptor<E, P>(
    selection: &ValidatedTargetSelection<E, P>,
    descriptor: &uob_application::TargetDescriptor,
    limits: TargetRuntimeLimits,
) -> Result<(), TargetSessionError> {
    if descriptor.kind != selection.catalog.kind
        || descriptor.instance_id != selection.target_id
        || descriptor.contract_version != ContractVersion::V1_INITIAL
        || descriptor.limits.maximum_message_bytes == 0
        || descriptor.limits.maximum_in_flight_deliveries == 0
        || descriptor.delivery_semantics.is_empty()
        || limits.maximum_command_bytes > descriptor.limits.maximum_message_bytes
        || limits.maximum_in_flight_deliveries > descriptor.limits.maximum_in_flight_deliveries
        || limits.maximum_in_flight_commands > descriptor.limits.maximum_in_flight_commands
        || (!descriptor.inbound_operations.is_empty()
            && (descriptor.limits.maximum_in_flight_commands == 0
                || limits.maximum_in_flight_commands == 0))
    {
        return Err(TargetSessionError::InvalidDescriptor);
    }
    Ok(())
}

fn flatten_join(
    result: Result<Result<(), TargetError>, JoinError>,
) -> Result<(), TargetSessionError> {
    result
        .map_err(TargetSessionError::Join)?
        .map_err(TargetSessionError::Target)
}

/// Target construction, validation, runtime, or supervision failure.
#[derive(Debug)]
pub enum TargetSessionError {
    /// Queue or runtime bounds are zero or exceed the shared host policy.
    InvalidLimits,
    /// The constructed target descriptor conflicts with selection or runtime limits.
    InvalidDescriptor,
    /// The selected factory could not construct the inactive target.
    Construction(ConfigurationError),
    /// The target session returned a classified failure.
    Target(TargetError),
    /// The spawned target task panicked or was unexpectedly cancelled.
    Join(JoinError),
    /// Graceful shutdown missed its supervisor-enforced duration.
    ShutdownDeadlineExceeded,
    /// The owning handle no longer contains a target task.
    SupervisorUnavailable,
}

impl fmt::Display for TargetSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("invalid target session limits"),
            Self::InvalidDescriptor => {
                formatter.write_str("target descriptor conflicts with selected session")
            }
            Self::Construction(error) => write!(formatter, "target construction failed: {error}"),
            Self::Target(error) => write!(formatter, "target session failed: {error}"),
            Self::Join(error) => write!(formatter, "target task failed: {error}"),
            Self::ShutdownDeadlineExceeded => {
                formatter.write_str("target shutdown deadline exceeded")
            }
            Self::SupervisorUnavailable => formatter.write_str("target supervisor unavailable"),
        }
    }
}

impl Error for TargetSessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Construction(error) => Some(error),
            Self::Target(error) => Some(error),
            Self::Join(error) => Some(error),
            _ => None,
        }
    }
}
