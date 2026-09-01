use super::{
    AdmissionError, RuntimeReservation, RuntimeResourceBudget, WorkClass, configuration_error,
};

/// Result of storing a latest-value telemetry item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryReplaceOutcome {
    Stored,
    Replaced,
}

/// One keyed latest-value slot that coalesces replaceable telemetry in place.
#[derive(Debug)]
pub struct ReplaceableTelemetrySlot<T> {
    class: WorkClass,
    value: Option<T>,
    reservation: Option<RuntimeReservation>,
}

impl<T> ReplaceableTelemetrySlot<T> {
    #[must_use]
    pub const fn new(class: WorkClass) -> Self {
        Self {
            class,
            value: None,
            reservation: None,
        }
    }

    /// Stores the first value or replaces the existing value without consuming another item slot.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when storing or growing the value exceeds a shared limit.
    pub fn replace(
        &mut self,
        budget: &RuntimeResourceBudget,
        value: T,
        encoded_bytes: usize,
    ) -> Result<TelemetryReplaceOutcome, AdmissionError> {
        if let Some(reservation) = &mut self.reservation {
            reservation.try_resize(encoded_bytes)?;
            self.value = Some(value);
            return Ok(TelemetryReplaceOutcome::Replaced);
        }
        let reservation = budget.try_reserve(self.class, encoded_bytes)?;
        self.value = Some(value);
        self.reservation = Some(reservation);
        Ok(TelemetryReplaceOutcome::Stored)
    }

    /// Removes the coalesced value and releases its shared reservation.
    pub fn take(&mut self) -> Option<T> {
        self.reservation.take();
        self.value.take()
    }
}

/// Decision after observing delivery pressure for one best-effort subscriber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaggingConsumerAction {
    Retain,
    Disconnect,
}

/// Bounded consecutive-full policy for disconnecting persistently slow consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaggingConsumer {
    maximum_consecutive_full: usize,
    consecutive_full: usize,
}

impl LaggingConsumer {
    /// Creates a slow-consumer policy with a non-zero consecutive-full threshold.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when the threshold is zero.
    pub fn new(maximum_consecutive_full: usize) -> Result<Self, AdmissionError> {
        if maximum_consecutive_full == 0 {
            return Err(configuration_error(0));
        }
        Ok(Self {
            maximum_consecutive_full,
            consecutive_full: 0,
        })
    }

    pub fn record_full(&mut self) -> LaggingConsumerAction {
        self.consecutive_full = self.consecutive_full.saturating_add(1);
        if self.consecutive_full >= self.maximum_consecutive_full {
            LaggingConsumerAction::Disconnect
        } else {
            LaggingConsumerAction::Retain
        }
    }

    pub fn record_progress(&mut self) {
        self.consecutive_full = 0;
    }
}
