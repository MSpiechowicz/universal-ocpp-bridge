use std::sync::{Arc, Mutex, PoisonError};

use uob_application::RuntimeReservation;

pub(super) struct SubscriberBudget {
    reservation: Mutex<RuntimeReservation>,
}

impl SubscriberBudget {
    pub(super) fn new(reservation: RuntimeReservation) -> Self {
        Self {
            reservation: Mutex::new(reservation),
        }
    }

    pub(super) fn reserve(self: &Arc<Self>, bytes: usize) -> Option<QueuedPayloadReservation> {
        let mut reservation = self
            .reservation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let new_bytes = reservation.bytes().checked_add(bytes)?;
        reservation.try_resize(new_bytes).ok()?;
        Some(QueuedPayloadReservation {
            budget: Arc::clone(self),
            bytes,
        })
    }

    fn release(&self, bytes: usize) {
        let mut reservation = self
            .reservation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let new_bytes = reservation.bytes().saturating_sub(bytes);
        let _ = reservation.try_resize(new_bytes);
    }
}

pub(super) struct QueuedPayloadReservation {
    budget: Arc<SubscriberBudget>,
    bytes: usize,
}

impl Drop for QueuedPayloadReservation {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}
