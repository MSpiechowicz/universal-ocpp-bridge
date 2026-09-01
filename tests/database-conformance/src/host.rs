use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

use tokio::sync::mpsc;
use uob_application::{
    DatabaseBatchReceiver, DatabaseDiagnostic, DatabaseDiagnosticDrop, DatabaseDiagnosticPort,
    DatabaseExportContext, DatabasePortError, DatabasePortErrorCode, DatabasePortFuture,
    DatabaseReportPort, DatabaseRuntimeLimits, DatabaseShutdown,
};
use uob_contracts::{ExportBatch, ExportReport, UtcTimestamp};

/// Independently bounded host channels supplied to one provider session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostCapacities {
    /// Canonical batches waiting for the provider.
    pub batches: usize,
    /// Critical outcomes waiting for durable host processing.
    pub reports: usize,
    /// Best-effort health and diagnostic records; zero disables diagnostics.
    pub diagnostics: usize,
}

impl HostCapacities {
    fn valid(self) -> bool {
        self.batches > 0 && self.reports > 0
    }
}

/// Failure observed while driving the reusable fake host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError {
    /// A bounded channel has no remaining capacity.
    Full,
    /// The provider session dropped its side of a channel.
    Closed,
    /// A required channel or runtime limit was zero.
    InvalidCapacity,
    /// The same logical batch is already awaiting a report.
    DuplicateBatch,
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "fake database host: {self:?}")
    }
}

impl Error for HostError {}

/// Driver retained by adapter tests after the provider receives its context.
pub struct FakeDatabaseHost {
    batches: mpsc::Sender<ExportBatch>,
    reports: mpsc::Receiver<ExportReport>,
    diagnostics: mpsc::Receiver<DatabaseDiagnostic>,
    expected: Arc<Mutex<BTreeMap<String, ExportBatch>>>,
    shutdown: Arc<ShutdownState>,
}

/// Provider context and its protocol-independent fake-host driver.
pub struct HostContext {
    /// Context passed unchanged to the provider under test.
    pub context: DatabaseExportContext,
    /// Host-side driver used by common behavioral scenarios.
    pub host: FakeDatabaseHost,
}

impl FakeDatabaseHost {
    /// Builds a host with bounded batch, report, and diagnostic channels.
    ///
    /// # Errors
    ///
    /// Batch/report capacities and every runtime limit must be non-zero.
    pub fn build(
        capacities: HostCapacities,
        limits: DatabaseRuntimeLimits,
        shutdown_deadline: UtcTimestamp,
    ) -> Result<HostContext, HostError> {
        if !capacities.valid()
            || limits.maximum_in_flight_batches == 0
            || limits.maximum_records_per_batch == 0
            || limits.maximum_batch_bytes == 0
        {
            return Err(HostError::InvalidCapacity);
        }
        let (batch_tx, batch_rx) = mpsc::channel(capacities.batches);
        let (report_tx, report_rx) = mpsc::channel(capacities.reports);
        let (diagnostic_tx, diagnostic_rx) = mpsc::channel(capacities.diagnostics.max(1));
        let expected = Arc::new(Mutex::new(BTreeMap::new()));
        let shutdown = Arc::new(ShutdownState::default());
        let context = DatabaseExportContext {
            batches: Box::pin(BoundedBatches(batch_rx)),
            critical_reports: Arc::new(ReportPort {
                sender: report_tx,
                expected: Arc::clone(&expected),
            }),
            diagnostics: Arc::new(DiagnosticPort {
                sender: diagnostic_tx,
                enabled: capacities.diagnostics > 0,
            }),
            limits,
            shutdown: Box::pin(ShutdownSignal(Arc::clone(&shutdown))),
            shutdown_deadline,
        };
        Ok(HostContext {
            context,
            host: Self {
                batches: batch_tx,
                reports: report_rx,
                diagnostics: diagnostic_rx,
                expected,
                shutdown,
            },
        })
    }

    /// Enqueues a canonical batch without waiting, exposing bounded backpressure.
    ///
    /// # Errors
    ///
    /// Rejects duplicate pending batch IDs, full capacity, or a stopped provider.
    ///
    /// # Panics
    ///
    /// Panics if another test thread poisoned the expected-batch ledger lock.
    pub fn try_send(&self, batch: ExportBatch) -> Result<(), HostError> {
        let key = batch.batch_id().as_str().to_owned();
        {
            let mut expected = self.expected.lock().expect("expected batches");
            if expected.contains_key(&key) {
                return Err(HostError::DuplicateBatch);
            }
            expected.insert(key.clone(), batch.clone());
        }
        self.batches.try_send(batch).map_err(|error| {
            self.expected.lock().expect("expected batches").remove(&key);
            match error {
                mpsc::error::TrySendError::Full(_) => HostError::Full,
                mpsc::error::TrySendError::Closed(_) => HostError::Closed,
            }
        })
    }

    /// Receives the next host-validated critical report.
    pub async fn next_report(&mut self) -> Option<ExportReport> {
        self.reports.recv().await
    }

    /// Receives the next best-effort provider diagnostic.
    pub async fn next_diagnostic(&mut self) -> Option<DatabaseDiagnostic> {
        self.diagnostics.recv().await
    }

    /// Returns the number of batches still awaiting valid terminal reports.
    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if another test thread poisoned the expected-batch ledger lock.
    pub fn pending_batches(&self) -> usize {
        self.expected.lock().expect("expected batches").len()
    }

    /// Requests graceful shutdown and wakes a pending provider task.
    pub fn request_shutdown(&self) {
        self.shutdown.request();
    }
}

struct BoundedBatches(mpsc::Receiver<ExportBatch>);

impl DatabaseBatchReceiver for BoundedBatches {
    fn poll_receive(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<ExportBatch>> {
        self.get_mut().0.poll_recv(context)
    }

    fn capacity(&self) -> usize {
        self.0.max_capacity()
    }

    fn backlog(&self) -> usize {
        self.0.len()
    }
}

struct ReportPort {
    sender: mpsc::Sender<ExportReport>,
    expected: Arc<Mutex<BTreeMap<String, ExportBatch>>>,
}

impl DatabaseReportPort for ReportPort {
    fn report(&self, report: ExportReport) -> DatabasePortFuture<'_> {
        Box::pin(async move {
            let key = report.batch_id().as_str().to_owned();
            let valid = self
                .expected
                .lock()
                .expect("expected batches")
                .get(&key)
                .is_some_and(|batch| report_matches(batch, &report));
            if !valid {
                return Err(DatabasePortError::new(
                    DatabasePortErrorCode::InvalidReport,
                    "report.unscheduled_or_mismatched",
                ));
            }
            self.sender.send(report).await.map_err(|_| {
                DatabasePortError::new(DatabasePortErrorCode::Closed, "report.host_closed")
            })?;
            self.expected.lock().expect("expected batches").remove(&key);
            Ok(())
        })
    }
}

fn report_matches(batch: &ExportBatch, report: &ExportReport) -> bool {
    report.destination() == batch.destination()
        && report.record_ids().iter().eq(batch
            .records()
            .iter()
            .map(|record| &record.metadata().identity))
}

struct DiagnosticPort {
    sender: mpsc::Sender<DatabaseDiagnostic>,
    enabled: bool,
}

impl DatabaseDiagnosticPort for DiagnosticPort {
    fn try_emit(&self, diagnostic: DatabaseDiagnostic) -> Result<(), DatabaseDiagnosticDrop> {
        if !self.enabled {
            return Err(DatabaseDiagnosticDrop::Disabled);
        }
        self.sender
            .try_send(diagnostic)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DatabaseDiagnosticDrop::Full,
                mpsc::error::TrySendError::Closed(_) => DatabaseDiagnosticDrop::Closed,
            })
    }
}

#[derive(Default)]
struct ShutdownState {
    requested: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl ShutdownState {
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        if let Some(waker) = self.waker.lock().expect("shutdown waker").take() {
            waker.wake();
        }
    }
}

struct ShutdownSignal(Arc<ShutdownState>);

impl DatabaseShutdown for ShutdownSignal {
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0.requested.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        *self.0.waker.lock().expect("shutdown waker") = Some(context.waker().clone());
        if self.0.requested.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
