use std::sync::Arc;

use tokio::sync::mpsc;
use uob_application::{
    AdmissionError, RuntimeReservation, RuntimeResourceBudget, StationInput, WorkClass,
};

mod runtime;

pub use runtime::{
    SpawnedStation, StationOutputKind, StationOutputs, StationTask, StationTaskError, spawn_station,
};

pub(super) struct Queued<T> {
    pub(super) value: T,
    pub(super) reservation: RuntimeReservation,
}

/// Failure to admit work to a station mailbox.
#[derive(Debug)]
pub enum StationSendError<T> {
    /// The shared process budget rejected the item before queueing.
    Rejected {
        /// Unqueued input returned to the caller.
        input: T,
        /// Exact shared limit that rejected the input.
        error: AdmissionError,
    },
    /// This station's bounded mailbox currently has no free slot.
    Full(T),
    /// The station task is shutting down or has stopped.
    Closed(T),
}

/// Cloneable producer for one station's serialized input mailbox.
pub struct StationHandle<O, C, R> {
    pub(super) sender: mpsc::Sender<Queued<StationInput<O, C, R>>>,
    pub(super) budget: Arc<RuntimeResourceBudget>,
}

impl<O, C, R> Clone for StationHandle<O, C, R> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            budget: Arc::clone(&self.budget),
        }
    }
}

impl<O, C, R> StationHandle<O, C, R> {
    /// Attempts to queue a charger observation without blocking the socket reader.
    ///
    /// # Errors
    ///
    /// Returns the original observation when a message, shared, station, or shutdown bound rejects
    /// it.
    pub fn try_observe(
        &self,
        observation: O,
        encoded_bytes: usize,
    ) -> Result<(), StationSendError<O>> {
        if let Err(error) = self.budget.validate_ocpp_message(encoded_bytes) {
            return Err(StationSendError::Rejected {
                input: observation,
                error,
            });
        }
        self.try_send(
            StationInput::Observation(observation),
            WorkClass::ChargerRequest,
            encoded_bytes,
        )
        .map_err(|error| error.map(into_observation))
    }

    /// Attempts to queue an application command without waiting for mailbox capacity.
    ///
    /// # Errors
    ///
    /// Returns the original command when a shared, station, or shutdown bound rejects it.
    pub fn try_command(&self, command: C, encoded_bytes: usize) -> Result<(), StationSendError<C>> {
        self.try_send(
            StationInput::Command(command),
            WorkClass::PendingRequest,
            encoded_bytes,
        )
        .map_err(|error| error.map(into_command))
    }

    /// Attempts to queue a protocol response without blocking the socket reader.
    ///
    /// # Errors
    ///
    /// Returns the original response when a message, shared, station, or shutdown bound rejects it.
    pub fn try_protocol_response(
        &self,
        response: R,
        encoded_bytes: usize,
    ) -> Result<(), StationSendError<R>> {
        if let Err(error) = self.budget.validate_ocpp_message(encoded_bytes) {
            return Err(StationSendError::Rejected {
                input: response,
                error,
            });
        }
        self.try_send(
            StationInput::ProtocolResponse(response),
            WorkClass::ChargerRequest,
            encoded_bytes,
        )
        .map_err(|error| error.map(into_protocol_response))
    }

    fn try_send(
        &self,
        input: StationInput<O, C, R>,
        class: WorkClass,
        encoded_bytes: usize,
    ) -> Result<(), StationSendError<StationInput<O, C, R>>> {
        let reservation = match self.budget.try_reserve(class, encoded_bytes) {
            Ok(reservation) => reservation,
            Err(error) => return Err(StationSendError::Rejected { input, error }),
        };
        self.sender
            .try_send(Queued {
                value: input,
                reservation,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(queued) => StationSendError::Full(queued.value),
                mpsc::error::TrySendError::Closed(queued) => StationSendError::Closed(queued.value),
            })
    }
}

impl<T> StationSendError<T> {
    fn map<U>(self, take: impl FnOnce(T) -> U) -> StationSendError<U> {
        match self {
            Self::Rejected { input, error } => StationSendError::Rejected {
                input: take(input),
                error,
            },
            Self::Full(input) => StationSendError::Full(take(input)),
            Self::Closed(input) => StationSendError::Closed(take(input)),
        }
    }
}

fn into_observation<O, C, R>(input: StationInput<O, C, R>) -> O {
    let StationInput::Observation(value) = input else {
        unreachable!("observation submission preserves its variant")
    };
    value
}

fn into_command<O, C, R>(input: StationInput<O, C, R>) -> C {
    let StationInput::Command(value) = input else {
        unreachable!("command submission preserves its variant")
    };
    value
}

fn into_protocol_response<O, C, R>(input: StationInput<O, C, R>) -> R {
    let StationInput::ProtocolResponse(value) = input else {
        unreachable!("response submission preserves its variant")
    };
    value
}

/// Stable station ordering metadata attached by the state-owning task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedStationOutput<T> {
    /// Monotonic receive order of the input that produced this output.
    pub input_sequence: u64,
    /// Stable order of this output among outputs produced by the same input.
    pub effect_sequence: usize,
    /// Target-neutral output value.
    pub value: T,
}

/// Consumer side of one bounded, size-accounted station output queue.
pub struct StationOutputReceiver<T> {
    pub(super) receiver: mpsc::Receiver<Queued<OrderedStationOutput<T>>>,
}

impl<T> StationOutputReceiver<T> {
    /// Receives the next ordered output, releasing its shared byte reservation.
    pub async fn receive(&mut self) -> Option<OrderedStationOutput<T>> {
        self.receiver.recv().await.map(|queued| queued.value)
    }

    /// Maximum number of queued outputs.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.receiver.max_capacity()
    }

    /// Current number of queued outputs.
    #[must_use]
    pub fn backlog(&self) -> usize {
        self.receiver.len()
    }
}
