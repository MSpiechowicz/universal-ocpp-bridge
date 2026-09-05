use std::{collections::HashMap, error::Error, fmt, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinError, JoinHandle},
    time::{MissedTickBehavior, interval},
};
use uob_application::{
    DeliveryAttempt, DeliveryAttemptResolution, DeliveryId, DeliveryOutcome, DeliveryReport,
    DeliverySemantic, Durability, PageLimit, PendingDelivery, PendingDeliveryQuery,
    ScheduledDelivery, StorageError, TargetDelivery, TargetDeliveryClass, TargetDeliveryStore,
    TargetMessage, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetReportPort,
};
use uob_contracts::{CommandResult, EventEnvelope, StationSnapshot, TraceRecord, UtcTimestamp};

use crate::TargetDeliveryIngress;

/// Adapter-owned serializable representation of one target-neutral message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum StoredTargetMessage<E> {
    /// Current station state.
    StationSnapshot(StationSnapshot),
    /// Durable domain event.
    DomainEvent(EventEnvelope<E>),
    /// Command lifecycle result.
    CommandResult(CommandResult),
    /// Explicitly redacted diagnostic record.
    Diagnostic(TraceRecord),
}

impl<E> From<TargetMessage<E>> for StoredTargetMessage<E> {
    fn from(value: TargetMessage<E>) -> Self {
        match value {
            TargetMessage::StationSnapshot(value) => Self::StationSnapshot(value),
            TargetMessage::DomainEvent(value) => Self::DomainEvent(value),
            TargetMessage::CommandResult(value) => Self::CommandResult(value),
            TargetMessage::Diagnostic(value) => Self::Diagnostic(value),
        }
    }
}

impl<E> From<StoredTargetMessage<E>> for TargetMessage<E> {
    fn from(value: StoredTargetMessage<E>) -> Self {
        match value {
            StoredTargetMessage::StationSnapshot(value) => Self::StationSnapshot(value),
            StoredTargetMessage::DomainEvent(value) => Self::DomainEvent(value),
            StoredTargetMessage::CommandResult(value) => Self::CommandResult(value),
            StoredTargetMessage::Diagnostic(value) => Self::Diagnostic(value),
        }
    }
}

/// Retry and completion rules for one persisted delivery class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeliveryRetryPolicy {
    /// Weakest exact target outcome that completes the outbox entry.
    pub completion: DeliverySemantic,
    /// Delay after the first retryable or insufficient report.
    pub initial_backoff: Duration,
    /// Upper bound for exponential retry delay.
    pub maximum_backoff: Duration,
}

/// Bounds and class-specific policies for one durable delivery worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetDeliveryWorkerOptions {
    /// Interval between bounded durable outbox reads.
    pub poll_interval: Duration,
    /// Maximum number of ready head-of-line entries read per poll.
    pub page_limit: PageLimit,
    /// Policy for critical durable events and results.
    pub durable: DeliveryRetryPolicy,
    /// Policy for replaceable latest-state work.
    pub replaceable_latest_state: DeliveryRetryPolicy,
}

/// Bounded report receiver owned by exactly one durable delivery worker.
pub struct TargetDeliveryReportReceiver(mpsc::Receiver<DeliveryReport>);

struct DurableReportPort(mpsc::Sender<DeliveryReport>);

impl TargetReportPort for DurableReportPort {
    fn report(&self, report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        Box::pin(async move {
            self.0.send(report).await.map_err(|_| {
                TargetPortError::new(
                    TargetPortErrorCode::Unavailable,
                    "target.delivery_worker_stopped",
                )
            })
        })
    }
}

/// Creates the critical report port before constructing a target session.
///
/// # Errors
///
/// A zero capacity cannot provide bounded progress and is rejected.
pub fn target_delivery_reports(
    capacity: usize,
) -> Result<(Arc<dyn TargetReportPort>, TargetDeliveryReportReceiver), TargetDeliveryWorkerError> {
    if capacity == 0 {
        return Err(TargetDeliveryWorkerError::InvalidOptions);
    }
    let (sender, receiver) = mpsc::channel(capacity);
    Ok((
        Arc::new(DurableReportPort(sender)),
        TargetDeliveryReportReceiver(receiver),
    ))
}

/// Owning handle for a host delivery worker that is independent of charging tasks.
pub struct TargetDeliveryWorkerTask {
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<Result<(), TargetDeliveryWorkerError>>>,
}

impl TargetDeliveryWorkerTask {
    /// Requests shutdown and waits for the worker to stop.
    ///
    /// # Errors
    ///
    /// Returns a storage, report-channel, join, or consumed-handle failure.
    pub async fn shutdown(self) -> Result<(), TargetDeliveryWorkerError> {
        self.shutdown_with_deadline(Duration::from_secs(20)).await
    }

    /// Drains the current operation within the host deadline, then cancels and joins.
    ///
    /// # Errors
    /// Reports worker errors or a missed deadline; durable pending deliveries are retained.
    pub async fn shutdown_with_deadline(
        mut self,
        deadline: Duration,
    ) -> Result<(), TargetDeliveryWorkerError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(join) = self.join.as_mut() else {
            return Err(TargetDeliveryWorkerError::SupervisorUnavailable);
        };
        if let Ok(result) = tokio::time::timeout(deadline, &mut *join).await {
            result.map_err(TargetDeliveryWorkerError::Join)?
        } else {
            join.abort();
            let _ = join.await;
            Err(TargetDeliveryWorkerError::ShutdownDeadlineExceeded)
        }
    }
}

impl Drop for TargetDeliveryWorkerTask {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

/// Starts a bounded worker for one selected target identity and configuration revision.
///
/// # Errors
///
/// Rejects zero polling/backoff values or an inverted backoff bound. Runtime storage failures are
/// returned by the task handle.
pub fn spawn_target_delivery_worker<E>(
    store: Arc<dyn TargetDeliveryStore<StoredTargetMessage<E>>>,
    ingress: TargetDeliveryIngress<E>,
    reports: TargetDeliveryReportReceiver,
    options: TargetDeliveryWorkerOptions,
) -> Result<TargetDeliveryWorkerTask, TargetDeliveryWorkerError>
where
    E: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    validate_options(options)?;
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let join = tokio::spawn(run_worker(
        store,
        ingress,
        reports.0,
        options,
        shutdown_receiver,
    ));
    Ok(TargetDeliveryWorkerTask {
        shutdown: Some(shutdown_sender),
        join: Some(join),
    })
}

async fn run_worker<E>(
    store: Arc<dyn TargetDeliveryStore<StoredTargetMessage<E>>>,
    ingress: TargetDeliveryIngress<E>,
    mut reports: mpsc::Receiver<DeliveryReport>,
    options: TargetDeliveryWorkerOptions,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), TargetDeliveryWorkerError>
where
    E: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let mut ticker = interval(options.poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut in_flight = HashMap::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            report = reports.recv() => {
                let Some(report) = report else {
                    return Err(TargetDeliveryWorkerError::ReportChannelClosed);
                };
                record_report(store.as_ref(), &mut in_flight, report, options).await?;
            }
            _ = ticker.tick() => {
                enqueue_ready(store.as_ref(), &ingress, &mut in_flight, options).await?;
            }
        }
    }
}

async fn enqueue_ready<E>(
    store: &dyn TargetDeliveryStore<StoredTargetMessage<E>>,
    ingress: &TargetDeliveryIngress<E>,
    in_flight: &mut HashMap<DeliveryId, InFlight>,
    options: TargetDeliveryWorkerOptions,
) -> Result<(), TargetDeliveryWorkerError>
where
    E: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let now = UtcTimestamp::new(time::OffsetDateTime::now_utc());
    expire_in_flight(store, in_flight, now).await?;
    let destination = ingress.destination();
    let ready = store
        .read_pending_deliveries(PendingDeliveryQuery {
            target_instance_id: destination.target_instance_id.clone(),
            target_configuration_revision: destination.configuration_revision,
            ready_at: now,
            limit: options.page_limit,
        })
        .await
        .map_err(TargetDeliveryWorkerError::Storage)?;
    for scheduled in ready {
        if in_flight.contains_key(&scheduled.delivery.delivery_id) {
            continue;
        }
        if scheduled.delivery.deadline <= now {
            expire(store, scheduled.delivery.delivery_id, now).await?;
            continue;
        }
        enqueue(ingress, in_flight, scheduled)?;
    }
    Ok(())
}

async fn expire_in_flight<E>(
    store: &dyn TargetDeliveryStore<StoredTargetMessage<E>>,
    in_flight: &mut HashMap<DeliveryId, InFlight>,
    now: UtcTimestamp,
) -> Result<(), TargetDeliveryWorkerError> {
    let expired = in_flight
        .iter()
        .filter(|(_, state)| state.deadline <= now)
        .map(|(delivery_id, _)| delivery_id.clone())
        .collect::<Vec<_>>();
    for delivery_id in expired {
        expire(store, delivery_id.clone(), now).await?;
        in_flight.remove(&delivery_id);
    }
    Ok(())
}

fn enqueue<E: Serialize>(
    ingress: &TargetDeliveryIngress<E>,
    in_flight: &mut HashMap<DeliveryId, InFlight>,
    scheduled: ScheduledDelivery<StoredTargetMessage<E>>,
) -> Result<(), TargetDeliveryWorkerError> {
    let encoded_bytes = serde_json::to_vec(&scheduled.delivery.payload)
        .map_err(|_| TargetDeliveryWorkerError::Encoding)?
        .len();
    let class = match scheduled.delivery.durability {
        Durability::Critical => TargetDeliveryClass::Durable,
        Durability::BestEffortTelemetry => TargetDeliveryClass::ReplaceableLatestState,
    };
    let delivery_id = scheduled.delivery.delivery_id.clone();
    let state = InFlight {
        attempt_count: scheduled.attempt_count,
        durability: scheduled.delivery.durability,
        deadline: scheduled.delivery.deadline,
    };
    let target_delivery = into_target_delivery(scheduled.delivery, class);
    if ingress.try_deliver(target_delivery, encoded_bytes).is_ok() {
        in_flight.insert(delivery_id, state);
    }
    Ok(())
}

fn into_target_delivery<E>(
    delivery: PendingDelivery<StoredTargetMessage<E>>,
    class: TargetDeliveryClass,
) -> TargetDelivery<E> {
    TargetDelivery {
        delivery_id: delivery.delivery_id,
        target_instance_id: delivery.target_instance_id,
        target_configuration_revision: delivery.target_configuration_revision,
        station_ordering_key: delivery.ordering_key,
        deadline: delivery.deadline,
        class,
        message: Arc::new(delivery.payload.into()),
    }
}

async fn record_report<E>(
    store: &dyn TargetDeliveryStore<StoredTargetMessage<E>>,
    in_flight: &mut HashMap<DeliveryId, InFlight>,
    report: DeliveryReport,
    options: TargetDeliveryWorkerOptions,
) -> Result<(), TargetDeliveryWorkerError> {
    let Some(state) = in_flight.remove(&report.delivery_id) else {
        let history = store
            .delivery_attempts(
                report.delivery_id.clone(),
                PageLimit::new(1).expect("one is a valid page limit"),
            )
            .await
            .map_err(TargetDeliveryWorkerError::Storage)?;
        return if history
            .iter()
            .any(|attempt| matches!(attempt.resolution, DeliveryAttemptResolution::Final))
        {
            Ok(())
        } else {
            Err(TargetDeliveryWorkerError::UnexpectedReport)
        };
    };
    let policy = match state.durability {
        Durability::Critical => options.durable,
        Durability::BestEffortTelemetry => options.replaceable_latest_state,
    };
    let resolution = if completes(policy.completion, &report.outcome) {
        DeliveryAttemptResolution::Final
    } else {
        let retry_at = retry_at(report.reported_at, policy, state.attempt_count);
        DeliveryAttemptResolution::RetryAt(retry_at.min(state.deadline))
    };
    store
        .record_delivery_attempt(DeliveryAttempt { report, resolution })
        .await
        .map_err(TargetDeliveryWorkerError::Storage)
}

async fn expire<E>(
    store: &dyn TargetDeliveryStore<StoredTargetMessage<E>>,
    delivery_id: DeliveryId,
    now: UtcTimestamp,
) -> Result<(), TargetDeliveryWorkerError> {
    store
        .record_delivery_attempt(DeliveryAttempt {
            report: DeliveryReport {
                delivery_id,
                outcome: DeliveryOutcome::PermanentFailure {
                    reason: "target.delivery_deadline_expired".to_owned(),
                },
                reported_at: now,
            },
            resolution: DeliveryAttemptResolution::Final,
        })
        .await
        .map_err(TargetDeliveryWorkerError::Storage)
}

fn completes(required: DeliverySemantic, outcome: &DeliveryOutcome) -> bool {
    matches!(outcome, DeliveryOutcome::PermanentFailure { .. })
        || matches!(outcome, DeliveryOutcome::Acknowledged { peer, scope }
            if !peer.trim().is_empty() && !scope.0.trim().is_empty())
        || matches!(
            (required, outcome),
            (
                DeliverySemantic::LocalExposure,
                DeliveryOutcome::LocallyExposed { .. }
            ) | (
                DeliverySemantic::UncertainHandoff,
                DeliveryOutcome::Uncertain { .. }
            )
        )
}

fn retry_at(
    reported_at: UtcTimestamp,
    policy: DeliveryRetryPolicy,
    attempt_count: u32,
) -> UtcTimestamp {
    let factor = 1_u32.checked_shl(attempt_count.min(31)).unwrap_or(u32::MAX);
    let delay = policy
        .initial_backoff
        .checked_mul(factor)
        .unwrap_or(policy.maximum_backoff)
        .min(policy.maximum_backoff);
    UtcTimestamp::new(reported_at.into_inner() + delay)
}

fn validate_options(options: TargetDeliveryWorkerOptions) -> Result<(), TargetDeliveryWorkerError> {
    for policy in [options.durable, options.replaceable_latest_state] {
        if policy.initial_backoff.is_zero() || policy.maximum_backoff < policy.initial_backoff {
            return Err(TargetDeliveryWorkerError::InvalidOptions);
        }
    }
    if options.poll_interval.is_zero() {
        return Err(TargetDeliveryWorkerError::InvalidOptions);
    }
    Ok(())
}

struct InFlight {
    attempt_count: u32,
    durability: Durability,
    deadline: UtcTimestamp,
}

/// Durable target delivery worker setup or runtime failure.
#[derive(Debug)]
pub enum TargetDeliveryWorkerError {
    /// A bound or retry policy is invalid.
    InvalidOptions,
    /// The durable operational store failed.
    Storage(StorageError),
    /// A target report did not correspond to scheduled in-flight work.
    UnexpectedReport,
    /// Every target report sender was closed while the worker remained active.
    ReportChannelClosed,
    /// A target-neutral message could not be measured for bounded admission.
    Encoding,
    /// The worker task panicked or was cancelled.
    Join(JoinError),
    /// The owning task handle was already consumed.
    SupervisorUnavailable,
    /// The current operation did not finish within the host deadline.
    ShutdownDeadlineExceeded,
}

impl fmt::Display for TargetDeliveryWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShutdownDeadlineExceeded => {
                formatter.write_str("target delivery shutdown deadline exceeded")
            }
            Self::InvalidOptions => formatter.write_str("invalid target delivery worker options"),
            Self::Storage(error) => write!(formatter, "target delivery storage failed: {error}"),
            Self::UnexpectedReport => formatter.write_str("unexpected target delivery report"),
            Self::ReportChannelClosed => {
                formatter.write_str("target delivery report channel closed")
            }
            Self::Encoding => formatter.write_str("target delivery encoding failed"),
            Self::Join(error) => write!(formatter, "target delivery worker failed: {error}"),
            Self::SupervisorUnavailable => {
                formatter.write_str("target delivery supervisor unavailable")
            }
        }
    }
}

impl Error for TargetDeliveryWorkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Join(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
