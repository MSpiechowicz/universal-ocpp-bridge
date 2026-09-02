use std::{
    future::Future,
    io::{self, Write},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use serde::Serialize;
use tokio::sync::{Semaphore, mpsc, oneshot};
use uob_application::{
    AccessPolicy, CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture,
    CommandAdmissionPort, DeliveryOutcome, DeliveryReport, RuntimeReservation,
    RuntimeResourceBudget, ScopedCommandAdmissionPort, TargetDelivery, TargetDeliveryReceiver,
    TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetReportPort, TargetRuntimeLimits,
    TargetShutdown, WorkClass,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandResult, ExternalCommand, Operation, TargetInstanceId,
};

pub(super) struct QueuedDelivery<E> {
    pub(super) delivery: TargetDelivery<E>,
    pub(super) _reservation: RuntimeReservation,
}

pub(super) struct BoundedDeliveryReceiver<E>(pub(super) mpsc::Receiver<QueuedDelivery<E>>);

impl<E: Send + Sync> TargetDeliveryReceiver<E> for BoundedDeliveryReceiver<E> {
    fn poll_receive(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<TargetDelivery<E>>> {
        self.get_mut()
            .0
            .poll_recv(context)
            .map(|queued| queued.map(|queued| queued.delivery))
    }

    fn capacity(&self) -> usize {
        self.0.max_capacity()
    }

    fn backlog(&self) -> usize {
        self.0.len()
    }
}

pub(super) struct ShutdownSignal(pub(super) oneshot::Receiver<()>);

impl TargetShutdown for ShutdownSignal {
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(context).map(|_| ())
    }
}

pub(super) fn guarded_commands<P>(
    inner: Arc<dyn CommandAdmissionPort<P>>,
    authorization: AccessPolicy,
    target_instance_id: TargetInstanceId,
    supported_operations: Vec<Operation>,
    limits: TargetRuntimeLimits,
) -> Arc<dyn CommandAdmissionPort<P>>
where
    P: Serialize + Send + 'static,
{
    let inner: Arc<dyn CommandAdmissionPort<P>> =
        Arc::new(ScopedCommandAdmissionPort::new(inner, authorization));
    Arc::new(GuardedCommandPort {
        inner,
        target_instance_id,
        supported_operations,
        maximum_command_bytes: limits.maximum_command_bytes,
        in_flight: Arc::new(Semaphore::new(limits.maximum_in_flight_commands)),
    })
}

struct GuardedCommandPort<P> {
    inner: Arc<dyn CommandAdmissionPort<P>>,
    target_instance_id: TargetInstanceId,
    supported_operations: Vec<Operation>,
    maximum_command_bytes: usize,
    in_flight: Arc<Semaphore>,
}

impl<P: Serialize + Send + 'static> CommandAdmissionPort<P> for GuardedCommandPort<P> {
    fn submit(&self, command: ExternalCommand<P>) -> CommandAdmissionFuture<'_, CommandResult> {
        let valid_origin = matches!(
            &command.origin,
            AuthenticatedCommandOrigin::Target { target_instance_id, .. }
                if target_instance_id == &self.target_instance_id
        );
        if !valid_origin {
            return command_error(
                CommandAdmissionErrorCode::Unauthorized,
                "target.command_origin_mismatch",
            );
        }
        if !self
            .supported_operations
            .contains(&command.request.operation.required_capability())
        {
            return command_error(
                CommandAdmissionErrorCode::Unsupported,
                "target.command_not_advertised",
            );
        }
        let mut encoded = EncodedSize::new(self.maximum_command_bytes);
        if serde_json::to_writer(&mut encoded, &command).is_err() && !encoded.exceeded() {
            return command_error(
                CommandAdmissionErrorCode::InvalidRequest,
                "target.command_encoding_failed",
            );
        }
        if encoded.exceeded() {
            return command_error(
                CommandAdmissionErrorCode::InvalidRequest,
                "target.command_too_large",
            );
        }
        let Ok(permit) = Arc::clone(&self.in_flight).try_acquire_owned() else {
            return command_error(CommandAdmissionErrorCode::Busy, "target.command_capacity");
        };
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let _permit = permit;
            inner.submit(command).await
        })
    }
}

struct EncodedSize {
    bytes: usize,
    maximum: usize,
    exceeded: bool,
}

impl EncodedSize {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: 0,
            maximum,
            exceeded: false,
        }
    }

    const fn exceeded(&self) -> bool {
        self.exceeded
    }
}

impl Write for EncodedSize {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("encoded command size overflow"));
        };
        if next > self.maximum {
            self.exceeded = true;
            return Err(io::Error::other("encoded command exceeds target limit"));
        }
        self.bytes = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn command_error<P>(
    code: CommandAdmissionErrorCode,
    context: &'static str,
) -> CommandAdmissionFuture<'static, P> {
    Box::pin(async move { Err(CommandAdmissionError::new(code, context)) })
}

pub(super) fn bounded_reports(
    inner: Arc<dyn TargetReportPort>,
    budget: Arc<RuntimeResourceBudget>,
    capacity: usize,
) -> Arc<dyn TargetReportPort> {
    Arc::new(BoundedReportPort {
        inner,
        budget,
        in_flight: Arc::new(Semaphore::new(capacity)),
    })
}

struct BoundedReportPort {
    inner: Arc<dyn TargetReportPort>,
    budget: Arc<RuntimeResourceBudget>,
    in_flight: Arc<Semaphore>,
}

impl TargetReportPort for BoundedReportPort {
    fn report(&self, report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        let inner = Arc::clone(&self.inner);
        let budget = Arc::clone(&self.budget);
        let in_flight = Arc::clone(&self.in_flight);
        Box::pin(async move {
            let permit = in_flight.acquire_owned().await.map_err(|_| {
                TargetPortError::new(TargetPortErrorCode::Unavailable, "target.report_closed")
            })?;
            let reservation = budget
                .try_reserve(WorkClass::CriticalReport, report_size(&report))
                .map_err(|_| {
                    TargetPortError::new(TargetPortErrorCode::Busy, "target.report_capacity")
                })?;
            let result = inner.report(report).await;
            drop(reservation);
            drop(permit);
            result
        })
    }
}

fn report_size(report: &DeliveryReport) -> usize {
    let outcome = match &report.outcome {
        DeliveryOutcome::LocallyExposed { surface } => surface.len(),
        DeliveryOutcome::Acknowledged { peer, scope } => peer.len().saturating_add(scope.0.len()),
        DeliveryOutcome::RetryableFailure { reason }
        | DeliveryOutcome::PermanentFailure { reason }
        | DeliveryOutcome::Uncertain { reason } => reason.len(),
    };
    report
        .delivery_id
        .as_str()
        .len()
        .saturating_add(outcome)
        .saturating_add(32)
}
