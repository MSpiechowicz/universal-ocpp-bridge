use std::{
    error::Error,
    fmt,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

use tokio::sync::{mpsc, oneshot};
use uob_application::{
    CommandAdmissionError, CommandAdmissionFuture, CommandAdmissionPort, DeliveryReport,
    DiagnosticDrop, TargetContext, TargetDelivery, TargetDeliveryReceiver, TargetDiagnostic,
    TargetDiagnosticPort, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryPort, TargetQueryResult, TargetReportPort, TargetRetainedEventStream,
    TargetRuntimeLimits, TargetShutdown,
};
use uob_contracts::{CommandResult, ExternalCommand, UtcTimestamp};

/// Independently bounded fake-host channels supplied to one target session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostCapacities {
    /// Host-owned outbound delivery queue.
    pub deliveries: usize,
    /// Target-originated commands awaiting host admission.
    pub commands: usize,
    /// Critical delivery reports awaiting durable processing.
    pub reports: usize,
    /// Best-effort diagnostics.
    pub diagnostics: usize,
}

impl HostCapacities {
    fn valid(self) -> bool {
        self.deliveries > 0 && self.commands > 0 && self.reports > 0
    }
}

/// Failure observed while driving the reusable fake host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError {
    /// A bounded channel has no remaining capacity.
    Full,
    /// The target session dropped its side of a channel.
    Closed,
    /// A zero capacity was supplied for a required channel.
    InvalidCapacity,
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "fake target host: {self:?}")
    }
}

impl Error for HostError {}

/// One target-originated command held until the fake host returns an authoritative result.
pub struct CommandSubmission<P> {
    /// Exact authenticated canonical command submitted by the adapter.
    pub command: ExternalCommand<P>,
    response: oneshot::Sender<Result<CommandResult, TargetPortError>>,
}

impl<P> CommandSubmission<P> {
    /// Completes host admission with a canonical result or structured rejection.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Closed`] when the target stopped awaiting the response.
    pub fn respond(self, result: Result<CommandResult, TargetPortError>) -> Result<(), HostError> {
        self.response.send(result).map_err(|_| HostError::Closed)
    }
}

/// Driver retained by adapter tests after the target receives its [`TargetContext`].
pub struct FakeTargetHost<E, P> {
    deliveries: mpsc::Sender<TargetDelivery<E>>,
    commands: mpsc::Receiver<CommandSubmission<P>>,
    reports: mpsc::Receiver<DeliveryReport>,
    diagnostics: mpsc::Receiver<TargetDiagnostic>,
    shutdown: Arc<ShutdownState>,
}

/// Target context and its protocol-independent fake-host driver.
pub struct HostContext<E, P> {
    /// Context passed unchanged to the concrete target under test.
    pub context: TargetContext<E, P>,
    /// Host-side driver used by the shared behavioral scenarios.
    pub host: FakeTargetHost<E, P>,
}

impl<E: Send + Sync + 'static, P: Send + 'static> FakeTargetHost<E, P> {
    /// Builds a bounded host around an already-scoped canonical query port.
    ///
    /// # Errors
    ///
    /// Required queue capacities must be non-zero.
    pub fn build(
        capacities: HostCapacities,
        queries: Arc<dyn TargetQueryPort<E>>,
        limits: TargetRuntimeLimits,
        shutdown_deadline: UtcTimestamp,
    ) -> Result<HostContext<E, P>, HostError> {
        if !capacities.valid() {
            return Err(HostError::InvalidCapacity);
        }
        let (delivery_tx, delivery_rx) = mpsc::channel(capacities.deliveries);
        let (command_tx, command_rx) = mpsc::channel(capacities.commands);
        let (report_tx, report_rx) = mpsc::channel(capacities.reports);
        let (diagnostic_tx, diagnostic_rx) = mpsc::channel(capacities.diagnostics.max(1));
        let shutdown = Arc::new(ShutdownState::default());
        let context = TargetContext {
            deliveries: Box::pin(BoundedDeliveries(delivery_rx)),
            queries,
            commands: Arc::new(CommandPort(command_tx)),
            critical_reports: Arc::new(ReportPort(report_tx)),
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
                deliveries: delivery_tx,
                commands: command_rx,
                reports: report_rx,
                diagnostics: diagnostic_rx,
                shutdown,
            },
        })
    }

    /// Enqueues without waiting, exposing bounded egress backpressure to the test.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Full`] at the configured bound or [`HostError::Closed`] after the
    /// target drops its receiver.
    pub fn try_deliver(&self, delivery: TargetDelivery<E>) -> Result<(), HostError> {
        self.deliveries
            .try_send(delivery)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => HostError::Full,
                mpsc::error::TrySendError::Closed(_) => HostError::Closed,
            })
    }

    /// Receives the next target-originated command for authorization and correlation assertions.
    pub async fn next_command(&mut self) -> Option<CommandSubmission<P>> {
        self.commands.recv().await
    }

    /// Receives the next critical delivery report independently of diagnostics.
    pub async fn next_report(&mut self) -> Option<DeliveryReport> {
        self.reports.recv().await
    }

    /// Receives the next best-effort diagnostic.
    pub async fn next_diagnostic(&mut self) -> Option<TargetDiagnostic> {
        self.diagnostics.recv().await
    }

    /// Requests graceful shutdown and wakes a pending target task.
    pub fn request_shutdown(&self) {
        self.shutdown.request();
    }
}

struct BoundedDeliveries<E>(mpsc::Receiver<TargetDelivery<E>>);

impl<E: Send + Sync> TargetDeliveryReceiver<E> for BoundedDeliveries<E> {
    fn poll_receive(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<TargetDelivery<E>>> {
        self.get_mut().0.poll_recv(context)
    }

    fn capacity(&self) -> usize {
        self.0.max_capacity()
    }

    fn backlog(&self) -> usize {
        self.0.len()
    }
}

struct CommandPort<P>(mpsc::Sender<CommandSubmission<P>>);

impl<P: Send + 'static> CommandAdmissionPort<P> for CommandPort<P> {
    fn submit(&self, command: ExternalCommand<P>) -> CommandAdmissionFuture<'_, CommandResult> {
        let sender = self.0.clone();
        Box::pin(async move {
            let (response, receiver) = oneshot::channel();
            sender
                .send(CommandSubmission { command, response })
                .await
                .map_err(|_| {
                    CommandAdmissionError::new(
                        uob_application::CommandAdmissionErrorCode::Unavailable,
                        "command.host_closed",
                    )
                })?;
            receiver
                .await
                .map_err(|_| {
                    CommandAdmissionError::new(
                        uob_application::CommandAdmissionErrorCode::Unavailable,
                        "command.response_dropped",
                    )
                })?
                .map_err(|error| {
                    CommandAdmissionError::new(map_port_error(error.code()), error.context())
                })
        })
    }
}

const fn map_port_error(
    code: uob_application::TargetPortErrorCode,
) -> uob_application::CommandAdmissionErrorCode {
    use uob_application::{CommandAdmissionErrorCode as Admission, TargetPortErrorCode as Target};

    match code {
        Target::Unauthorized => Admission::Unauthorized,
        Target::Unsupported => Admission::Unsupported,
        Target::Expired => Admission::Expired,
        Target::Busy => Admission::Busy,
        Target::Unavailable | Target::CursorExpired => Admission::Unavailable,
        Target::InvalidRequest => Admission::InvalidRequest,
    }
}

/// Maps common admission failures back to the target-facing scoped-port contract.
#[must_use]
pub fn target_port_error_from_admission(error: &CommandAdmissionError) -> TargetPortError {
    let code = match error.code() {
        uob_application::CommandAdmissionErrorCode::Unauthorized => {
            TargetPortErrorCode::Unauthorized
        }
        uob_application::CommandAdmissionErrorCode::Expired => TargetPortErrorCode::Expired,
        uob_application::CommandAdmissionErrorCode::Unsupported => TargetPortErrorCode::Unsupported,
        uob_application::CommandAdmissionErrorCode::Busy => TargetPortErrorCode::Busy,
        uob_application::CommandAdmissionErrorCode::Unavailable => TargetPortErrorCode::Unavailable,
        uob_application::CommandAdmissionErrorCode::PolicyRejected
        | uob_application::CommandAdmissionErrorCode::InvalidRequest => {
            TargetPortErrorCode::InvalidRequest
        }
    };
    TargetPortError::new(code, error.context())
}

struct ReportPort(mpsc::Sender<DeliveryReport>);

impl TargetReportPort for ReportPort {
    fn report(&self, report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        let sender = self.0.clone();
        Box::pin(async move {
            sender
                .send(report)
                .await
                .map_err(|_| closed("report.host_closed"))
        })
    }
}

struct DiagnosticPort {
    sender: mpsc::Sender<TargetDiagnostic>,
    enabled: bool,
}

impl TargetDiagnosticPort for DiagnosticPort {
    fn try_emit(&self, diagnostic: TargetDiagnostic) -> Result<(), DiagnosticDrop> {
        if !self.enabled {
            return Err(DiagnosticDrop::Disabled);
        }
        self.sender
            .try_send(diagnostic)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DiagnosticDrop::Full,
                mpsc::error::TrySendError::Closed(_) => DiagnosticDrop::Closed,
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

impl TargetShutdown for ShutdownSignal {
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

fn closed(context: &'static str) -> TargetPortError {
    TargetPortError::new(TargetPortErrorCode::Unavailable, context)
}

/// Explicit query port for read-only targets or scenarios without query permission.
#[derive(Default)]
pub struct UnsupportedQueryPort;

impl<E: Send> TargetQueryPort<E> for UnsupportedQueryPort {
    fn query(&self, _query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<E>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "query.not_granted",
            ))
        })
    }

    fn subscribe_retained_events(
        &self,
        _query: uob_application::RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<E>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "query.subscription_not_granted",
            ))
        })
    }
}
