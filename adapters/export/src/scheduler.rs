//! Optional, single-session export delivery supervisor.

mod delivery;
mod load;
mod ports;
mod retry;

use parking_lot::Mutex;
use std::{error::Error, fmt, future::Future, sync::Arc, time::Duration};

use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use uob_application::{
    ConfigurationError, DatabaseAcknowledgementScope, DatabaseHealth, DatabaseHealthState,
    DatabaseProviderDescriptor, DeduplicationCapability, ExportSpool, ExportSpoolError,
    ExportSpoolErrorCode, ExportSpoolNamespace, RuntimeResourceBudget, TransactionCapability,
};
use uob_contracts::ExportBatchId;

use crate::{ValidatedDataExport, ValidatedProviderSelection};
use delivery::{DeliveryOutcome, deliver};
use ports::StopSignal;
use retry::{RetryPolicy, retry_after_failure};

const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const IO_DEADLINE: Duration = Duration::from_secs(5);
const MAX_BATCH_BYTES: usize = 256 * 1024;
const MAX_BATCH_RECORDS: u16 = 100;

/// Export supervision failure, never returned to the charging admission path.
#[derive(Debug)]
pub enum ExportSchedulerError {
    Configuration(ConfigurationError),
    Spool(ExportSpoolError),
    InvalidProvider(&'static str),
    InvalidPending(&'static str),
    MemoryPressure,
    Worker(tokio::task::JoinError),
}

impl fmt::Display for ExportSchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(f, "export provider construction: {error}"),
            Self::Spool(error) => write!(f, "export spool: {error}"),
            Self::InvalidProvider(reason) => write!(f, "export provider: {reason}"),
            Self::InvalidPending(reason) => write!(f, "export pending: {reason}"),
            Self::MemoryPressure => f.write_str("export batch memory admission unavailable"),
            Self::Worker(error) => write!(f, "export worker: {error}"),
        }
    }
}

impl Error for ExportSchedulerError {}

/// Snapshot of the optional component. An outage never changes core charging health.
#[derive(Clone, Debug)]
pub struct ExportSchedulerHealth {
    pub provider: DatabaseHealth,
    pub last_confirmed_batch: Option<ExportBatchId>,
    pub confirmed_records: u64,
    pub retry_count: u64,
}

impl Default for ExportSchedulerHealth {
    fn default() -> Self {
        Self {
            provider: DatabaseHealth {
                state: DatabaseHealthState::Starting,
                batch_backlog: 0,
                in_flight_batches: 0,
                active_connections: 0,
                reason: None,
            },
            last_confirmed_batch: None,
            confirmed_records: 0,
            retry_count: 0,
        }
    }
}

type SharedHealth = Arc<Mutex<ExportSchedulerHealth>>;

fn update(health: &SharedHealth, f: impl FnOnce(&mut ExportSchedulerHealth)) {
    let mut state = health.lock();
    f(&mut state);
}

/// Owns the sole worker, including its retry clock; drop cancels even a hung session.
pub struct ExportSchedulerHandle {
    stop: StopSignal,
    task: JoinHandle<Result<(), ExportSchedulerError>>,
    health: SharedHealth,
}

impl ExportSchedulerHandle {
    #[must_use]
    pub fn health(&self) -> ExportSchedulerHealth {
        self.health.lock().clone()
    }

    /// Requests a graceful stop, then aborts the single worker after the fixed grace period.
    ///
    /// # Errors
    /// Returns an optional export error; no charger response depends on this operation.
    pub async fn shutdown(mut self) -> Result<(), ExportSchedulerError> {
        self.stop.stop();
        let Ok(result) = timeout(
            ports::SHUTDOWN_DEADLINE + Duration::from_millis(100),
            &mut self.task,
        )
        .await
        else {
            self.task.abort();
            let _ = (&mut self.task).await;
            return Err(ExportSchedulerError::InvalidProvider("shutdown.deadline"));
        };
        result.map_err(ExportSchedulerError::Worker)?
    }
}

impl Drop for ExportSchedulerHandle {
    fn drop(&mut self) {
        self.stop.stop();
        self.task.abort();
    }
}

/// Creates no worker, provider, or polling loop while export is disabled.
pub struct ExportScheduler;

impl ExportScheduler {
    /// Starts a single serial worker for an already validated provider selection.
    ///
    /// # Errors
    /// Rejects a mismatched destination or provider kind before spawning any worker.
    pub fn start(
        selection: ValidatedDataExport,
        namespace: ExportSpoolNamespace,
        spool: Arc<dyn ExportSpool>,
        resources: RuntimeResourceBudget,
    ) -> Result<Option<ExportSchedulerHandle>, ExportSchedulerError> {
        let ValidatedDataExport::Enabled(selection) = selection else {
            return Ok(None);
        };
        if namespace.destination != selection.destination
            || namespace.provider_kind != selection.catalog.kind
        {
            return Err(ExportSchedulerError::InvalidProvider("namespace.mismatch"));
        }
        let stop = StopSignal::new();
        let supervisor_stop = stop.clone();
        let health = Arc::new(Mutex::new(ExportSchedulerHealth::default()));
        let task_health = health.clone();
        let task = tokio::spawn(async move {
            let result = run(
                selection,
                namespace,
                spool,
                resources,
                stop.clone(),
                task_health.clone(),
            )
            .await;
            if let Err(error) = &result {
                update(&task_health, |state| {
                    state.provider.state = DatabaseHealthState::Degraded;
                    state.provider.in_flight_batches = 0;
                    state.provider.reason = Some(
                        match error {
                            ExportSchedulerError::Spool(_) => "spool.failure",
                            ExportSchedulerError::Configuration(_) => "provider.configuration",
                            ExportSchedulerError::InvalidProvider(_) => "provider.invalid",
                            ExportSchedulerError::InvalidPending(_) => "spool.invalid_pending",
                            ExportSchedulerError::MemoryPressure => "spool.memory_pressure",
                            ExportSchedulerError::Worker(_) => "worker.failure",
                        }
                        .into(),
                    );
                });
            }
            result
        });
        Ok(Some(ExportSchedulerHandle {
            stop: supervisor_stop,
            task,
            health,
        }))
    }
}

fn validate_descriptor(
    descriptor: &DatabaseProviderDescriptor,
    selection: &ValidatedProviderSelection,
) -> Result<(), ExportSchedulerError> {
    if descriptor.kind.as_str() != selection.catalog.kind
        || descriptor.instance_id != selection.destination.destination_id
        || descriptor.limits.maximum_records_per_batch == 0
        || descriptor.limits.maximum_batch_bytes == 0
        || descriptor.limits.maximum_in_flight_batches == 0
        || descriptor.deduplication != DeduplicationCapability::StableRecordIdentity
        || descriptor.transactions != TransactionCapability::AtomicBatch
        || descriptor.acknowledgement_scope != DatabaseAcknowledgementScope::AtomicRemoteCommit
    {
        return Err(ExportSchedulerError::InvalidProvider(
            "descriptor.unsupported",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct BatchLimits {
    records: uob_application::PageLimit,
    bytes: usize,
}

fn batch_limits(
    selection: &ValidatedProviderSelection,
) -> Result<BatchLimits, ExportSchedulerError> {
    // Select limits before pinning a durable claim. A restarted worker may find
    // an older claim made under a different provider configuration.
    let descriptor = selection
        .create()
        .map_err(ExportSchedulerError::Configuration)?
        .descriptor();
    validate_descriptor(&descriptor, selection)?;
    let max_records =
        usize::from(MAX_BATCH_RECORDS).min(descriptor.limits.maximum_records_per_batch);
    let records = uob_application::PageLimit::new(
        u16::try_from(max_records).expect("bounded by scheduler record limit"),
    )
    .expect("validated page limit");
    Ok(BatchLimits {
        records,
        bytes: MAX_BATCH_BYTES.min(descriptor.limits.maximum_batch_bytes),
    })
}

async fn run(
    selection: ValidatedProviderSelection,
    namespace: ExportSpoolNamespace,
    spool: Arc<dyn ExportSpool>,
    resources: RuntimeResourceBudget,
    stop: StopSignal,
    health: SharedHealth,
) -> Result<(), ExportSchedulerError> {
    let limits = batch_limits(&selection)?;
    let mut retry = RetryPolicy::default();
    let mut cleanup_degraded = false;
    let Some(recovered) = spool_operation(&stop, &health, "spool.status_timeout", false, || {
        spool.status(namespace.clone())
    })
    .await?
    else {
        update(&health, |state| {
            state.provider.state = DatabaseHealthState::Stopped;
        });
        return Ok(());
    };
    update(&health, |state| {
        state.last_confirmed_batch = recovered.last_confirmed_batch;
        state.confirmed_records = recovered.confirmed_records;
        state.provider.batch_backlog = usize::from(recovered.pending_records != 0);
    });

    while !stop.is_stopped() {
        let claim = match claim_next(
            spool.as_ref(),
            &namespace,
            limits.records,
            limits.bytes,
            &stop,
            &health,
        )
        .await?
        {
            ClaimOutcome::Stopped => break,
            ClaimOutcome::Empty => {
                update(&health, |state| {
                    if !cleanup_degraded {
                        state.provider.state = DatabaseHealthState::Ready;
                        state.provider.reason = None;
                    }
                    state.provider.batch_backlog = 0;
                });
                tokio::select! { () = stop.wait() => break, () = sleep(FLUSH_INTERVAL) => {} }
                continue;
            }
            ClaimOutcome::Claimed(claim) => claim,
        };

        let (batch, _batch_reservation, encoded_bytes) = match load_claim(
            spool.as_ref(),
            &namespace,
            &resources,
            &claim,
            limits,
            &stop,
            &health,
        )
        .await?
        {
            LoadOutcome::Stopped => break,
            LoadOutcome::MemoryPressure => {
                tokio::select! { () = stop.wait() => break, () = sleep(FLUSH_INTERVAL) => {} }
                continue;
            }
            LoadOutcome::Loaded(batch, reservation, bytes) => (batch, reservation, bytes),
        };

        match deliver(
            &selection,
            &namespace,
            spool.as_ref(),
            batch,
            encoded_bytes,
            &stop,
            &health,
        )
        .await?
        {
            DeliveryOutcome::Stopped | DeliveryOutcome::PermanentFailure => break,
            DeliveryOutcome::Committed => {
                cleanup_degraded = false;
                retry.reset();
                continue;
            }
            DeliveryOutcome::CommittedWithShutdownTimeout => {
                cleanup_degraded = true;
                retry.reset();
                continue;
            }
            DeliveryOutcome::Retry => {}
        }
        if retry_after_failure(&mut retry, &stop, &health).await {
            break;
        }
    }
    update(&health, |state| {
        state.provider.state = DatabaseHealthState::Stopped;
        state.provider.in_flight_batches = 0;
    });
    Ok(())
}

enum ClaimOutcome {
    Stopped,
    Empty,
    Claimed(uob_application::ExportDeliveryClaim),
}

async fn claim_next(
    spool: &dyn ExportSpool,
    namespace: &ExportSpoolNamespace,
    limit: uob_application::PageLimit,
    max_bytes: usize,
    stop: &StopSignal,
    health: &SharedHealth,
) -> Result<ClaimOutcome, ExportSchedulerError> {
    match spool_operation(stop, health, "spool.claim_timeout", true, || {
        spool.claim_delivery(namespace.clone(), limit, max_bytes)
    })
    .await?
    {
        None => Ok(ClaimOutcome::Stopped),
        Some(None) => Ok(ClaimOutcome::Empty),
        Some(Some(claim)) => Ok(ClaimOutcome::Claimed(claim)),
    }
}

// Busy is transient for all spool operations; Backpressure is retried only for
// claim memory reservation. Other operations may report permanent resource limits.
async fn spool_operation<T, F, Fut>(
    stop: &StopSignal,
    health: &SharedHealth,
    timeout_reason: &'static str,
    retry_backpressure: bool,
    mut operation: F,
) -> Result<Option<T>, ExportSchedulerError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ExportSpoolError>>,
{
    loop {
        let result = tokio::select! {
            biased;
            () = stop.wait() => return Ok(None),
            result = timeout(IO_DEADLINE, operation()) => result,
        };
        match result {
            Ok(Ok(value)) => return Ok(Some(value)),
            Ok(Err(error))
                if error.code() == ExportSpoolErrorCode::Busy
                    || (retry_backpressure
                        && error.code() == ExportSpoolErrorCode::Backpressure) =>
            {
                update(health, |state| {
                    state.retry_count += 1;
                    if error.code() == ExportSpoolErrorCode::Busy {
                        state.provider.state = DatabaseHealthState::Reconnecting;
                        state.provider.reason = Some("spool.busy".into());
                    } else {
                        state.provider.state = DatabaseHealthState::Degraded;
                        state.provider.reason = Some("spool.memory_pressure".into());
                    }
                });
                tokio::select! {
                    () = stop.wait() => return Ok(None),
                    () = sleep(FLUSH_INTERVAL) => {}
                }
            }
            Ok(Err(error)) => return Err(ExportSchedulerError::Spool(error)),
            Err(_) => return Err(ExportSchedulerError::InvalidPending(timeout_reason)),
        }
    }
}

enum LoadOutcome {
    Stopped,
    MemoryPressure,
    Loaded(
        uob_contracts::ExportBatch,
        uob_application::RuntimeReservation,
        usize,
    ),
}

async fn load_claim(
    spool: &dyn ExportSpool,
    namespace: &ExportSpoolNamespace,
    resources: &RuntimeResourceBudget,
    claim: &uob_application::ExportDeliveryClaim,
    limits: BatchLimits,
    stop: &StopSignal,
    health: &SharedHealth,
) -> Result<LoadOutcome, ExportSchedulerError> {
    let loaded = loop {
        let result = tokio::select! {
            biased;
            () = stop.wait() => return Ok(LoadOutcome::Stopped),
            result = timeout(IO_DEADLINE, load::load_batch(spool, namespace, resources, claim, usize::from(limits.records.get()), limits.bytes)) => {
                result.map_err(|_| ExportSchedulerError::InvalidPending("spool.read_timeout"))?
            }
        };
        if matches!(&result, Err(ExportSchedulerError::Spool(error)) if matches!(error.code(), ExportSpoolErrorCode::Busy | ExportSpoolErrorCode::Backpressure))
        {
            update(health, |state| {
                state.retry_count += 1;
                if matches!(&result, Err(ExportSchedulerError::Spool(error)) if error.code() == ExportSpoolErrorCode::Busy)
                {
                    state.provider.state = DatabaseHealthState::Reconnecting;
                    state.provider.reason = Some("spool.busy".into());
                } else {
                    state.provider.state = DatabaseHealthState::Degraded;
                    state.provider.reason = Some("spool.memory_pressure".into());
                }
            });
            tokio::select! {
                () = stop.wait() => return Ok(LoadOutcome::Stopped),
                () = sleep(FLUSH_INTERVAL) => {}
            }
        } else {
            break result;
        }
    };
    match loaded {
        Ok((batch, reservation, bytes)) => Ok(LoadOutcome::Loaded(batch, reservation, bytes)),
        Err(ExportSchedulerError::MemoryPressure) => {
            update(health, |state| {
                state.provider.state = DatabaseHealthState::Degraded;
                state.provider.reason = Some("spool.memory_pressure".into());
            });
            Ok(LoadOutcome::MemoryPressure)
        }
        Err(error) => Err(error),
    }
}
