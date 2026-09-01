/// Input serialized by one station-owned state task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StationInput<O, C, R> {
    /// A charger-originated observation, including keepalives and state updates.
    Observation(O),
    /// Application work addressed to the station.
    Command(C),
    /// A charger response that completes or advances previously dispatched work.
    ProtocolResponse(R),
}

/// One output with an explicit encoded-size charge against the shared runtime budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SizedStationOutput<T> {
    /// Target-neutral output value.
    pub value: T,
    /// Encoded bytes retained while this output is queued.
    pub encoded_bytes: usize,
}

impl<T> SizedStationOutput<T> {
    /// Creates a size-accounted station output.
    #[must_use]
    pub const fn new(value: T, encoded_bytes: usize) -> Self {
        Self {
            value,
            encoded_bytes,
        }
    }
}

/// Effects produced atomically by one state-machine input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationEffects<T, D> {
    /// Ordered application transitions for persistence and publication.
    pub transitions: Vec<SizedStationOutput<T>>,
    /// Protocol work for a socket-owning adapter to dispatch without blocking this actor.
    pub dispatches: Vec<SizedStationOutput<D>>,
}

impl<T, D> StationEffects<T, D> {
    /// Creates an input result with no externally visible effects.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            transitions: Vec::new(),
            dispatches: Vec::new(),
        }
    }
}

/// Target-neutral synchronous transition boundary owned by one station task.
///
/// Protocol waiting never occurs in this method. A dispatched request is completed by a later
/// [`StationInput::ProtocolResponse`], allowing observations and keepalives to progress meanwhile.
pub trait StationStateMachine: Send + 'static {
    /// Charger observation accepted by the state machine.
    type Observation: Send + 'static;
    /// Application command accepted by the state machine.
    type Command: Send + 'static;
    /// Charger response to earlier protocol dispatch.
    type ProtocolResponse: Send + 'static;
    /// Ordered target-neutral application transition.
    type Transition: Send + 'static;
    /// Protocol-adapter work item.
    type Dispatch: Send + 'static;
    /// Sanitized state transition failure.
    type Error: Send + 'static;

    /// Applies exactly one input while holding exclusive ownership of station state.
    ///
    /// # Errors
    ///
    /// Returns a sanitized state-machine error without publishing partial effects.
    fn apply(
        &mut self,
        input: StationInput<Self::Observation, Self::Command, Self::ProtocolResponse>,
    ) -> Result<StationEffects<Self::Transition, Self::Dispatch>, Self::Error>;
}
