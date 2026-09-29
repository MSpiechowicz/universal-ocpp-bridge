use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures_util::task::AtomicWaker;
use tokio::{sync::mpsc, time::timeout};
use uob_application::{
    DatabaseBatchReceiver, DatabaseDiagnostic, DatabaseDiagnosticDrop, DatabaseDiagnosticPort,
    DatabaseError, DatabaseExportContext, DatabasePortError, DatabasePortErrorCode,
    DatabasePortFuture, DatabaseProvider, DatabaseProviderDescriptor, DatabaseReportPort,
    DatabaseRuntimeLimits, DatabaseShutdown,
};
use uob_contracts::{ExportBatch, ExportRecordIdentity, ExportReport, UtcTimestamp};

use super::{IO_DEADLINE, MAX_BATCH_BYTES, MAX_BATCH_RECORDS};

pub(super) const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(super) struct StopSignal(Arc<StopInner>);

struct StopInner {
    stopped: AtomicBool,
    waker: AtomicWaker,
}

impl StopSignal {
    pub(super) fn new() -> Self {
        Self(Arc::new(StopInner {
            stopped: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }))
    }

    pub(super) fn stop(&self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0.waker.wake();
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }

    pub(super) async fn wait(&self) {
        std::future::poll_fn(|context| self.poll(context)).await;
    }

    fn poll(&self, context: &mut Context<'_>) -> Poll<()> {
        if self.is_stopped() {
            return Poll::Ready(());
        }
        self.0.waker.register(context.waker());
        if self.is_stopped() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl DatabaseShutdown for StopSignal {
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        self.poll(context)
    }
}

struct Receiver(mpsc::Receiver<ExportBatch>);

impl DatabaseBatchReceiver for Receiver {
    fn poll_receive(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<ExportBatch>> {
        self.0.poll_recv(context)
    }

    fn capacity(&self) -> usize {
        1
    }

    fn backlog(&self) -> usize {
        self.0.len()
    }
}

struct Reports {
    expected: Vec<ExportRecordIdentity>,
    batch_id: uob_contracts::ExportBatchId,
    destination: uob_contracts::ExportDestination,
    descriptor: DatabaseProviderDescriptor,
    sender: mpsc::Sender<ExportReport>,
}

impl DatabaseReportPort for Reports {
    fn report(&self, report: ExportReport) -> DatabasePortFuture<'_> {
        Box::pin(async move {
            if report.batch_id() != &self.batch_id
                || report.destination() != &self.destination
                || report.record_ids() != self.expected.as_slice()
                || self.descriptor.validate_report(&report).is_err()
            {
                return Err(DatabasePortError::new(
                    DatabasePortErrorCode::InvalidReport,
                    "report.claim_mismatch",
                ));
            }
            self.sender.try_send(report).map_err(|_| {
                DatabasePortError::new(DatabasePortErrorCode::Busy, "report.channel_unavailable")
            })
        })
    }
}

struct SilentDiagnostics;

impl DatabaseDiagnosticPort for SilentDiagnostics {
    fn try_emit(&self, _diagnostic: DatabaseDiagnostic) -> Result<(), DatabaseDiagnosticDrop> {
        Err(DatabaseDiagnosticDrop::Disabled)
    }
}

pub(super) enum AttemptResult {
    Reported {
        report: ExportReport,
        shutdown_timed_out: bool,
    },
    Failed(DatabaseError),
    TimedOut,
    ShutdownTimedOut,
    Stopped,
}

pub(super) struct Attempt {
    provider: Box<dyn DatabaseProvider>,
    descriptor: DatabaseProviderDescriptor,
    batch: ExportBatch,
}

impl Attempt {
    pub(super) fn new(
        provider: Box<dyn DatabaseProvider>,
        descriptor: DatabaseProviderDescriptor,
        batch: ExportBatch,
    ) -> Self {
        Self {
            provider,
            descriptor,
            batch,
        }
    }

    pub(super) async fn run(self, host_stop: &StopSignal) -> AttemptResult {
        let (batch_sender, batch_receiver) = mpsc::channel(1);
        let (report_sender, mut report_receiver) = mpsc::channel(1);
        let reports = Reports {
            expected: self
                .batch
                .records()
                .iter()
                .map(|item| item.metadata().identity.clone())
                .collect(),
            batch_id: self.batch.batch_id().clone(),
            destination: self.batch.destination().clone(),
            descriptor: self.descriptor,
            sender: report_sender,
        };
        // The host owns one batch and one result slot; no second in-flight batch can exist.
        batch_sender
            .try_send(self.batch)
            .expect("one empty batch slot");
        let provider_stop = StopSignal::new();
        let context = DatabaseExportContext {
            batches: Box::pin(Receiver(batch_receiver)),
            critical_reports: Arc::new(reports),
            diagnostics: Arc::new(SilentDiagnostics),
            limits: DatabaseRuntimeLimits {
                maximum_in_flight_batches: 1,
                maximum_records_per_batch: usize::from(MAX_BATCH_RECORDS),
                maximum_batch_bytes: MAX_BATCH_BYTES,
            },
            shutdown: Box::pin(provider_stop.clone()),
            shutdown_deadline: UtcTimestamp::new(
                time::OffsetDateTime::now_utc() + time::Duration::seconds(2),
            ),
        };
        let mut task = self.provider.run(context);
        let (event, completed) = tokio::select! {
            biased;
            () = host_stop.wait() => (AttemptResult::Stopped, false),
            Some(report) = report_receiver.recv() => (
                AttemptResult::Reported { report, shutdown_timed_out: false },
                false,
            ),
            result = &mut task => (match report_receiver.try_recv() {
                Ok(report) => AttemptResult::Reported {
                    report,
                    shutdown_timed_out: false,
                },
                Err(_) => match result {
                    Ok(()) => AttemptResult::TimedOut,
                    Err(error) => AttemptResult::Failed(error),
                },
            }, true),
            () = tokio::time::sleep(IO_DEADLINE) => (AttemptResult::TimedOut, false),
        };
        provider_stop.stop();
        drop(batch_sender);
        if completed {
            return event;
        }
        if matches!(event, AttemptResult::Stopped) {
            let _ = timeout(SHUTDOWN_DEADLINE, task).await;
            return AttemptResult::Stopped;
        }
        let shutdown_timed_out = timeout(SHUTDOWN_DEADLINE, task).await.is_err();
        // A report may also arrive while a timed-out provider handles shutdown.
        // Never replace a confirmed remote result with a cleanup failure.
        match (event, report_receiver.try_recv()) {
            (AttemptResult::Reported { report, .. }, _) | (_, Ok(report)) => {
                AttemptResult::Reported {
                    report,
                    shutdown_timed_out,
                }
            }
            (_, Err(_)) if shutdown_timed_out => AttemptResult::ShutdownTimedOut,
            (event, Err(_)) => event,
        }
    }
}
