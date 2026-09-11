use std::sync::{
    Arc, MutexGuard, TryLockError,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use super::{Inner, RuntimeReservation, RuntimeResourceBudget, Usage, WorkClass, admission};

impl RuntimeResourceBudget {
    /// Attempts optional diagnostic admission without waiting for another producer.
    ///
    /// Returns `None` for contention, exhausted capacity, or any class other than
    /// [`WorkClass::Diagnostic`] and [`WorkClass::CaptureTrace`]. Dropping a returned
    /// reservation also never waits for the shared budget lock.
    #[must_use]
    pub fn try_reserve_diagnostic(
        &self,
        class: WorkClass,
        bytes: usize,
    ) -> Option<RuntimeReservation> {
        if !matches!(class, WorkClass::Diagnostic | WorkClass::CaptureTrace) {
            return None;
        }
        let mut usage = try_lock_usage(&self.inner)?;
        admission::reserve_with_usage(self.inner.limits, &mut usage, class, bytes).ok()?;
        Some(RuntimeReservation {
            inner: Some(Arc::clone(&self.inner)),
            class,
            bytes,
        })
    }

    /// Reports pressure without waiting for the shared budget lock.
    ///
    /// Contention or use of at least three quarters of the noncritical payload
    /// allowance is pressure, so optional producers can shed detail before allocating it.
    #[must_use]
    pub fn try_diagnostic_pressure(&self) -> bool {
        let Some(usage) = try_lock_usage(&self.inner) else {
            return true;
        };
        let optional_bytes = self
            .inner
            .limits
            .aggregate_queued_payload_bytes
            .saturating_sub(self.inner.limits.reserved_critical_payload_bytes);
        let threshold = optional_bytes - optional_bytes / 4;
        usage.queued_payload_bytes >= threshold
    }
}

fn try_lock_usage(inner: &Inner) -> Option<MutexGuard<'_, Usage>> {
    let mut usage = match inner.usage.try_lock() {
        Ok(usage) => usage,
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        Err(TryLockError::WouldBlock) => return None,
    };
    inner.diagnostics.drain_released(&mut usage);
    Some(usage)
}

#[derive(Debug, Default)]
struct DeferredRelease {
    bytes: AtomicUsize,
    items: AtomicUsize,
}

#[derive(Debug, Default)]
pub(super) struct DiagnosticAccounting {
    diagnostic: DeferredRelease,
    capture: DeferredRelease,
    dropped: AtomicU64,
}

impl DiagnosticAccounting {
    /// Defers subtracting released capacity so eviction never waits for a budget reader.
    /// Counters only contain previously admitted capacity and therefore remain bounded
    /// by the configured byte and item limits. A racing drain can leave either counter
    /// charged until the next admission or snapshot; it cannot free live capacity.
    pub(super) fn release(&self, class: WorkClass, bytes: usize) {
        let released = if class == WorkClass::CaptureTrace {
            &self.capture
        } else {
            &self.diagnostic
        };
        released.bytes.fetch_add(bytes, Ordering::Relaxed);
        released.items.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn drain_released(&self, usage: &mut Usage) {
        for (class, released) in [
            (WorkClass::Diagnostic, &self.diagnostic),
            (WorkClass::CaptureTrace, &self.capture),
        ] {
            let bytes = released.bytes.swap(0, Ordering::Relaxed);
            let items = released.items.swap(0, Ordering::Relaxed);
            usage.queued_payload_bytes = usage.queued_payload_bytes.saturating_sub(bytes);
            usage.items[class.index()] = usage.items[class.index()].saturating_sub(items);
            if class == WorkClass::CaptureTrace {
                usage.trace_ring_bytes = usage.trace_ring_bytes.saturating_sub(bytes);
            }
        }
    }

    pub(super) fn record_drop(&self) {
        let _ = self
            .dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(1))
            });
    }

    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}
