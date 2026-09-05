use std::{error::Error, fmt, sync::Arc, time::Duration};

use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinError, JoinHandle},
    time::timeout,
};
use uob_application::{
    AdmissionError, AdmissionLimit, RuntimeResourceBudget, SizedStationOutput, StationAdmission,
    StationInput, StationStateMachine, WorkClass,
};

use super::{OrderedStationOutput, Queued, StationHandle, StationOutputReceiver};

/// Separate ordered application and protocol surfaces for a running station.
pub struct StationOutputs<T, D> {
    /// Transitions to persist and publish through application-owned ports.
    pub transitions: StationOutputReceiver<T>,
    /// Work for the socket-owning protocol adapter.
    pub dispatches: StationOutputReceiver<D>,
}

/// Queue that prevented a station effect from being published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StationOutputKind {
    /// Ordered application transition queue.
    Transition,
    /// Protocol dispatch queue.
    ProtocolDispatch,
}

/// Terminal failure reported by a supervised station task.
#[derive(Debug)]
pub enum StationTaskError<E> {
    /// The station state machine rejected an input.
    State(E),
    /// Shared byte or item capacity rejected a produced effect.
    OutputRejected {
        /// Saturated output surface.
        output: StationOutputKind,
        /// Exact shared resource rejection.
        error: AdmissionError,
    },
    /// A bounded output consumer was full or had closed.
    OutputUnavailable(StationOutputKind),
    /// The spawned actor panicked or was cancelled unexpectedly.
    Join(JoinError),
    /// Graceful shutdown did not finish inside the supplied deadline.
    ShutdownDeadlineExceeded,
    /// The owning supervisor no longer has a task handle.
    SupervisorUnavailable,
}

impl<E: fmt::Display> fmt::Display for StationTaskError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => write!(formatter, "station transition failed: {error}"),
            Self::OutputRejected { output, error } => {
                write!(formatter, "station {output:?} rejected: {error:?}")
            }
            Self::OutputUnavailable(output) => write!(formatter, "station {output:?} unavailable"),
            Self::Join(error) => write!(formatter, "station task failed: {error}"),
            Self::ShutdownDeadlineExceeded => {
                formatter.write_str("station shutdown deadline exceeded")
            }
            Self::SupervisorUnavailable => formatter.write_str("station supervisor unavailable"),
        }
    }
}

impl<E: Error + 'static> Error for StationTaskError<E> {}

/// Owning supervisor handle for exactly one station task.
pub struct StationTask<E> {
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<Result<(), StationTaskError<E>>>>,
}

impl<E: Send + 'static> StationTask<E> {
    /// Waits until the station stops without requesting shutdown.
    ///
    /// # Errors
    ///
    /// Returns the state, queue, panic, or cancellation failure that stopped the task.
    pub async fn wait(mut self) -> Result<(), StationTaskError<E>> {
        let shutdown = self.shutdown.take();
        let Some(join_handle) = self.join.as_mut() else {
            return Err(StationTaskError::SupervisorUnavailable);
        };
        let result = flatten_join(join_handle.await);
        drop(shutdown);
        result
    }

    /// Requests graceful cancellation and enforces a maximum shutdown duration.
    ///
    /// # Errors
    ///
    /// Returns the task failure, or aborts and reports a missed shutdown deadline.
    pub async fn shutdown(mut self, deadline: Duration) -> Result<(), StationTaskError<E>> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(join_handle) = self.join.as_mut() else {
            return Err(StationTaskError::SupervisorUnavailable);
        };
        if let Ok(result) = timeout(deadline, &mut *join_handle).await {
            flatten_join(result)
        } else {
            join_handle.abort();
            let _ = join_handle.await;
            Err(StationTaskError::ShutdownDeadlineExceeded)
        }
    }
}

impl<E> Drop for StationTask<E> {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

/// Handle, output queues, and supervisor returned for one spawned station.
pub type SpawnedStation<M> = (
    StationHandle<
        <M as StationStateMachine>::Observation,
        <M as StationStateMachine>::Command,
        <M as StationStateMachine>::ProtocolResponse,
    >,
    StationOutputs<<M as StationStateMachine>::Transition, <M as StationStateMachine>::Dispatch>,
    StationTask<<M as StationStateMachine>::Error>,
);

type StationIngress<M> = mpsc::Receiver<
    Queued<
        StationInput<
            <M as StationStateMachine>::Observation,
            <M as StationStateMachine>::Command,
            <M as StationStateMachine>::ProtocolResponse,
        >,
    >,
>;

/// Starts one admitted state-owning actor and its bounded application/protocol queues.
///
/// # Errors
///
/// Returns [`AdmissionError`] when a capacity is zero or the connected-station limit is exhausted.
pub fn spawn_station<M>(
    machine: M,
    budget: Arc<RuntimeResourceBudget>,
    ingress_capacity: usize,
    output_capacity: usize,
) -> Result<SpawnedStation<M>, AdmissionError>
where
    M: StationStateMachine,
{
    if ingress_capacity == 0 || output_capacity == 0 {
        return Err(AdmissionError {
            limit: AdmissionLimit::Configuration,
            maximum: 0,
            requested: ingress_capacity.min(output_capacity),
        });
    }
    let admission = budget.admit_station()?;
    let (ingress_sender, ingress_receiver) = mpsc::channel(ingress_capacity);
    let (transition_sender, transition_receiver) = mpsc::channel(output_capacity);
    let (dispatch_sender, dispatch_receiver) = mpsc::channel(output_capacity);
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let actor_budget = Arc::clone(&budget);
    let join = tokio::spawn(run_station(
        machine,
        ingress_receiver,
        transition_sender,
        dispatch_sender,
        shutdown_receiver,
        actor_budget,
        admission,
    ));
    Ok((
        StationHandle {
            sender: ingress_sender,
            budget,
        },
        StationOutputs {
            transitions: StationOutputReceiver {
                receiver: transition_receiver,
            },
            dispatches: StationOutputReceiver {
                receiver: dispatch_receiver,
            },
        },
        StationTask {
            shutdown: Some(shutdown_sender),
            join: Some(join),
        },
    ))
}

async fn run_station<M: StationStateMachine>(
    mut machine: M,
    mut ingress: StationIngress<M>,
    transitions: mpsc::Sender<Queued<OrderedStationOutput<M::Transition>>>,
    dispatches: mpsc::Sender<Queued<OrderedStationOutput<M::Dispatch>>>,
    mut shutdown: oneshot::Receiver<()>,
    budget: Arc<RuntimeResourceBudget>,
    _admission: StationAdmission,
) -> Result<(), StationTaskError<M::Error>> {
    let mut input_sequence = 0_u64;
    loop {
        let queued = tokio::select! {
            biased;
            _ = &mut shutdown => {
                ingress.close();
                return Ok(());
            }
            queued = ingress.recv() => queued,
        };
        let Some(queued) = queued else {
            return Ok(());
        };
        let effects = machine
            .apply(queued.value)
            .map_err(StationTaskError::State)?;
        drop(queued.reservation);
        publish_outputs(
            &transitions,
            &budget,
            WorkClass::CriticalReport,
            StationOutputKind::Transition,
            input_sequence,
            effects.transitions,
        )?;
        publish_outputs(
            &dispatches,
            &budget,
            WorkClass::PendingRequest,
            StationOutputKind::ProtocolDispatch,
            input_sequence,
            effects.dispatches,
        )?;
        input_sequence = input_sequence.saturating_add(1);
    }
}

fn publish_outputs<T, E>(
    sender: &mpsc::Sender<Queued<OrderedStationOutput<T>>>,
    budget: &RuntimeResourceBudget,
    class: WorkClass,
    kind: StationOutputKind,
    input_sequence: u64,
    outputs: Vec<SizedStationOutput<T>>,
) -> Result<(), StationTaskError<E>> {
    for (effect_sequence, output) in outputs.into_iter().enumerate() {
        let reservation = budget
            .try_reserve(class, output.encoded_bytes)
            .map_err(|error| StationTaskError::OutputRejected {
                output: kind,
                error,
            })?;
        sender
            .try_send(Queued {
                value: OrderedStationOutput {
                    input_sequence,
                    effect_sequence,
                    value: output.value,
                },
                reservation,
            })
            .map_err(|_| StationTaskError::OutputUnavailable(kind))?;
    }
    Ok(())
}

fn flatten_join<E>(
    result: Result<Result<(), StationTaskError<E>>, JoinError>,
) -> Result<(), StationTaskError<E>> {
    result.map_err(StationTaskError::Join)?
}
