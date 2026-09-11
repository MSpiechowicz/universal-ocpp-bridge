use super::{CaptureError, CaptureFilter, CaptureManager, TraceRead, lock, reap};
use std::{sync::atomic::Ordering, time::Instant};

/// Revocable sink permission, not a copy of retained trace memory. Sinks must check each read.
#[derive(Debug)]
pub struct CaptureLease {
    pub(super) manager: CaptureManager,
    pub(super) id: u64,
    pub(super) export_id: Option<u64>,
    pub(super) filter: CaptureFilter,
}

impl CaptureLease {
    /// The complete trusted selection authorized when this lease was issued.
    #[must_use]
    pub const fn filter(&self) -> &CaptureFilter {
        &self.filter
    }

    /// Reads at most one shared record, with explicit bounded-window and drop evidence.
    /// Every read rechecks this session and the subscriber or export lifetime.
    /// # Errors
    /// Returns `Gone` after capture revocation, expiry, or export expiry.
    pub fn read_after(&self, after: Option<u64>) -> Result<TraceRead, CaptureError> {
        let mut state = lock(&self.manager.state);
        reap(&mut state, Instant::now());
        let session = state
            .session
            .as_ref()
            .filter(|s| {
                s.status.id == self.id
                    && self.export_id.map_or(!s.stopped, |id| {
                        s.exports.iter().any(|(value, _)| *value == id)
                    })
            })
            .ok_or(CaptureError::Gone)?;
        let dropped = self
            .manager
            .dropped
            .load(Ordering::Relaxed)
            .saturating_sub(session.dropped_at_start);
        Ok(session
            .ring
            .read_after(after, state.next_trace_sequence, dropped))
    }

    /// Checks session lifetime and exact record selection for every streamed/exported record.
    #[must_use]
    pub fn permits(&self, record: &CaptureFilter) -> bool {
        let mut state = lock(&self.manager.state);
        reap(&mut state, Instant::now());
        state.session.as_ref().is_some_and(|s| {
            s.status.id == self.id
                && s.status.filter.includes(record)
                && self
                    .export_id
                    .map_or(!s.stopped, |id| s.exports.iter().any(|(v, _)| *v == id))
        })
    }
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        let mut state = lock(&self.manager.state);
        if let Some(session) = state.session.as_mut().filter(|s| s.status.id == self.id) {
            if let Some(id) = self.export_id {
                session.exports.retain(|(v, _)| *v != id);
            } else {
                session.subscribers = session.subscribers.saturating_sub(1);
            }
        }
        reap(&mut state, Instant::now());
    }
}
