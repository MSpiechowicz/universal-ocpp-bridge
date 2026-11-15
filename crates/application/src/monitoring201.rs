//! Application-owned OCPP 2.0.1 `NotifyEvent` evidence (N07). Events are charger facts,
//! journaled as observed and never turned into configuration, policy or commands.
//!
//! Storage retention treats every journal event as critical and cannot shed it, so flooding is
//! stopped here, before the write: [`MonitoringEventGate201`] bounds each record, spends a
//! per-station budget that outlives any one connection, and counts what it sheds so the next
//! durable record reports the loss.
mod persistence;
pub use persistence::record_monitoring_events_201;

use crate::{DiagnosticDropReason, RuntimeResourceBudget};
use std::time::Duration;
use uob_contracts::{
    MONITORING_EVENT_EVIDENCE_LIMIT_201, MONITORING_EVENTS_PER_RECORD_LIMIT_201,
    MonitoringEvent201, MonitoringEventOrdering201, MonitoringEventRecord201, UtcTimestamp,
};

#[cfg(test)]
mod tests;

/// The already schema-validated and sanitized content of one `NotifyEvent` CALL. It may hold
/// more events than one journal record keeps; the gate bounds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotifyEventObservation201 {
    pub generated_at: UtcTimestamp,
    pub seq_no: u32,
    /// The charger announced that more events follow.
    pub tbc: bool,
    pub events: Vec<MonitoringEvent201>,
}

/// Fixed-window journal budget for one station.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitoringEventLimits201 {
    pub window: Duration,
    /// Journal records admitted per window.
    pub records: u32,
    /// Encoded evidence bytes admitted per window.
    pub bytes: usize,
}

impl Default for MonitoringEventLimits201 {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            records: 60,
            bytes: 1024 * 1024,
        }
    }
}

/// Outcome of offering one `NotifyEvent` CALL to the gate. The CALL is acknowledged either way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MonitoringEventDecision201 {
    /// Journal `record`, then call [`MonitoringEventGate201::committed`] with `receipt`. Dropping
    /// the receipt after a failed write keeps the ordering state and the shed counts, so the
    /// charger's retry is judged exactly like the failed attempt.
    Record {
        record: MonitoringEventRecord201,
        receipt: MonitoringEventReceipt201,
    },
    /// Over budget, nothing is written. The totals are everything shed since the last
    /// durable record, including this call.
    Shed { calls: u32, events: u32 },
}

/// Proof of what a record carried, applied to the gate once that record is durable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitoringEventReceipt201 {
    next_expected: u32,
    shed_calls: u32,
    shed_events: u32,
}

/// Per-station `NotifyEvent` flood state and per-connection ordering. The budget window and the
/// shed counts belong to the station and survive reconnects; only the ordering restarts with a
/// connection ([`Self::connection_started`]). It reads no clock; callers pass `now`.
#[derive(Clone, Debug)]
pub struct MonitoringEventGate201 {
    limits: MonitoringEventLimits201,
    /// Sequence number the next message must carry to be the successor: 0 at the start of a
    /// series, one past the previous message while it announced more.
    expected: u32,
    window_start: Option<UtcTimestamp>,
    window_records: u32,
    window_bytes: usize,
    shed_calls: u32,
    shed_events: u32,
}

impl Default for MonitoringEventGate201 {
    fn default() -> Self {
        Self::new(MonitoringEventLimits201::default())
    }
}

impl MonitoringEventGate201 {
    #[must_use]
    pub const fn new(limits: MonitoringEventLimits201) -> Self {
        Self {
            limits,
            expected: 0,
            window_start: None,
            window_records: 0,
            window_bytes: 0,
            shed_calls: 0,
            shed_events: 0,
        }
    }

    /// A new connection starts a new series of messages. Only the ordering restarts: the budget
    /// already spent in the window and the shed counts not yet reported stay with the station,
    /// so reconnecting never refreshes the flood budget.
    pub const fn connection_started(&mut self) {
        self.expected = 0;
    }

    /// Judges ordering and budget for one CALL. A record spends its budget here, so a storage
    /// failure cannot be used to bypass the flood limit. A shed call advances the ordering at
    /// once, because the charger was acknowledged and will not repeat it.
    pub fn admit(
        &mut self,
        observation: NotifyEventObservation201,
        now: UtcTimestamp,
    ) -> MonitoringEventDecision201 {
        self.roll_window(now);

        let ordering = self.ordering(observation.seq_no);
        let next_expected = next_expected(observation.seq_no, observation.tbc);
        let received = observation.events.len();

        if self.window_records >= self.limits.records {
            return self.shed(received, next_expected);
        }

        let record = bounded_record(observation, ordering, self.shed_calls, self.shed_events);
        let size = record_len(&record);
        if self.window_bytes.saturating_add(size) > self.limits.bytes {
            return self.shed(received, next_expected);
        }

        self.window_records = self.window_records.saturating_add(1);
        self.window_bytes = self.window_bytes.saturating_add(size);
        let receipt = MonitoringEventReceipt201 {
            next_expected,
            shed_calls: record.shed_calls,
            shed_events: record.shed_events,
        };

        MonitoringEventDecision201::Record { record, receipt }
    }

    /// Confirms the record behind `receipt` is durable: advances the ordering and removes the
    /// shed counts it reported.
    pub const fn committed(&mut self, receipt: MonitoringEventReceipt201) {
        self.expected = receipt.next_expected;
        self.shed_calls = self.shed_calls.saturating_sub(receipt.shed_calls);
        self.shed_events = self.shed_events.saturating_sub(receipt.shed_events);
    }

    fn ordering(&self, seq_no: u32) -> MonitoringEventOrdering201 {
        if seq_no == self.expected {
            MonitoringEventOrdering201::InSequence
        } else if seq_no == 0 {
            MonitoringEventOrdering201::Restarted
        } else {
            MonitoringEventOrdering201::Gap {
                expected: self.expected,
            }
        }
    }

    fn shed(&mut self, events: usize, next_expected: u32) -> MonitoringEventDecision201 {
        self.expected = next_expected;
        self.shed_calls = self.shed_calls.saturating_add(1);
        self.shed_events = self.shed_events.saturating_add(saturating_u32(events));

        MonitoringEventDecision201::Shed {
            calls: self.shed_calls,
            events: self.shed_events,
        }
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
            self.window_bytes = 0;
        }
    }
}

/// Counts one shed call as best-effort diagnostic loss on the shared resource signal.
pub fn record_monitoring_event_shed_201(budget: &RuntimeResourceBudget) {
    budget.record_diagnostic_drop(DiagnosticDropReason::Full);
}

const fn next_expected(seq_no: u32, tbc: bool) -> u32 {
    if tbc { seq_no.saturating_add(1) } else { 0 }
}

/// Keeps a native-order prefix of at most 32 events whose encoding fits the evidence bound,
/// and counts every event it leaves out.
fn bounded_record(
    observation: NotifyEventObservation201,
    ordering: MonitoringEventOrdering201,
    shed_calls: u32,
    shed_events: u32,
) -> MonitoringEventRecord201 {
    let received = observation.events.len();
    let mut record = MonitoringEventRecord201 {
        generated_at: observation.generated_at,
        seq_no: observation.seq_no,
        tbc: observation.tbc,
        ordering,
        events: Vec::new(),
        // Starts at the widest value so the size estimate never undercounts.
        events_omitted: saturating_u32(received),
        shed_calls,
        shed_events,
    };
    let mut size = record_len(&record);

    for event in observation
        .events
        .into_iter()
        .take(MONITORING_EVENTS_PER_RECORD_LIMIT_201)
    {
        // One byte for the separating comma.
        let added = event_len(&event).saturating_add(1);
        if size.saturating_add(added) > MONITORING_EVENT_EVIDENCE_LIMIT_201 {
            break;
        }
        size += added;
        record.events.push(event);
    }
    record.events_omitted = saturating_u32(received - record.events.len());

    record
}

fn record_len(record: &MonitoringEventRecord201) -> usize {
    serde_json::to_vec(record).map_or(usize::MAX, |encoded| encoded.len())
}

fn event_len(event: &MonitoringEvent201) -> usize {
    serde_json::to_vec(event).map_or(usize::MAX, |encoded| encoded.len())
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
