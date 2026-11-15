//! Per-station flood gate for `SecurityEventNotification`. Journal events cannot be shed by
//! storage retention, so the budget is spent here, before the write. The window and the shed
//! count belong to the station and survive reconnects; the gate reads no clock.
use crate::{DiagnosticDropReason, RuntimeResourceBudget};
use std::time::Duration;
use uob_contracts::UtcTimestamp;

/// Fixed-window journal budget for one station.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecurityEventLimits16 {
    pub window: Duration,
    /// Journal records admitted per window.
    pub records: u32,
}

impl Default for SecurityEventLimits16 {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            records: 60,
        }
    }
}

/// Outcome of offering one call to the gate. The call is acknowledged either way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityEventDecision16 {
    /// Journal the event with `shed_calls` in its record, then call
    /// [`SecurityEventGate16::committed`] with `receipt`. Dropping the receipt after a failed
    /// write keeps the shed count for the next record.
    Store {
        shed_calls: u32,
        receipt: SecurityEventReceipt16,
    },
    /// Over budget, nothing is written. `calls` is everything shed since the last durable
    /// record, including this call.
    Shed { calls: u32 },
}

/// Proof of how many shed calls a record reported, applied once that record is durable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecurityEventReceipt16 {
    shed_calls: u32,
}

/// Per-station flood state.
#[derive(Clone, Debug)]
pub struct SecurityEventGate16 {
    limits: SecurityEventLimits16,
    window_start: Option<UtcTimestamp>,
    window_records: u32,
    shed_calls: u32,
}

impl Default for SecurityEventGate16 {
    fn default() -> Self {
        Self::new(SecurityEventLimits16::default())
    }
}

impl SecurityEventGate16 {
    #[must_use]
    pub const fn new(limits: SecurityEventLimits16) -> Self {
        Self {
            limits,
            window_start: None,
            window_records: 0,
            shed_calls: 0,
        }
    }

    /// Judges one call. A stored record spends its budget here, so a storage failure cannot be
    /// used to bypass the flood limit.
    pub fn admit(&mut self, now: UtcTimestamp) -> SecurityEventDecision16 {
        self.roll_window(now);

        if self.window_records >= self.limits.records {
            self.shed_calls = self.shed_calls.saturating_add(1);

            return SecurityEventDecision16::Shed {
                calls: self.shed_calls,
            };
        }

        self.window_records = self.window_records.saturating_add(1);

        SecurityEventDecision16::Store {
            shed_calls: self.shed_calls,
            receipt: SecurityEventReceipt16 {
                shed_calls: self.shed_calls,
            },
        }
    }

    /// Confirms the record behind `receipt` is durable and removes the shed count it reported.
    pub const fn committed(&mut self, receipt: SecurityEventReceipt16) {
        self.shed_calls = self.shed_calls.saturating_sub(receipt.shed_calls);
    }

    /// Starts a new window on first use, after the window elapsed, or when the clock moved back.
    fn roll_window(&mut self, now: UtcTimestamp) {
        let window = time::Duration::try_from(self.limits.window).unwrap_or(time::Duration::MAX);
        let expired = self.window_start.is_none_or(|start| {
            let elapsed = now.into_inner() - start.into_inner();
            elapsed.is_negative() || elapsed >= window
        });

        if expired {
            self.window_start = Some(now);
            self.window_records = 0;
        }
    }
}

/// Counts one shed call as best-effort diagnostic loss on the shared resource signal.
pub fn record_security_event_shed_16(budget: &RuntimeResourceBudget) {
    budget.record_diagnostic_drop(DiagnosticDropReason::Full);
}
