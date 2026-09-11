use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::{RuntimeReservation, RuntimeResourceBudget, SanitizedDiagnostic, WorkClass};

use super::CaptureError;

/// Includes all encoded JSON metadata and excerpts, after central redaction.
pub const MAX_TRACE_RECORD_BYTES: usize = 64 * 1024;
const MAX_RING_BYTES: usize = 8 * 1024 * 1024;
const MAX_RING_RECORDS: usize = 2_000;

#[derive(Clone, Copy, Debug)]
pub(super) struct RingLimits {
    bytes: usize,
    records: usize,
}

impl RingLimits {
    pub(super) fn new(bytes: usize, records: usize) -> Result<Self, CaptureError> {
        if bytes == 0 || bytes > MAX_RING_BYTES || records == 0 || records > MAX_RING_RECORDS {
            return Err(CaptureError::Invalid);
        }
        Ok(Self { bytes, records })
    }

    pub(super) fn for_resources(resources: &RuntimeResourceBudget) -> Self {
        Self {
            bytes: resources.limits().trace_ring_bytes.min(MAX_RING_BYTES),
            records: resources
                .limits()
                .queues
                .capture_records
                .min(MAX_RING_RECORDS),
        }
    }
}

/// One centrally encoded record; readers share both bytes and the same resource reservation.
#[derive(Debug)]
pub struct RetainedTrace {
    /// Monotonically increasing across captures in this process.
    pub sequence: u64,
    /// The inert representation that already crossed the redaction boundary.
    pub diagnostic: SanitizedDiagnostic,
    _reservation: RuntimeReservation,
    _memory: RecordMemory,
}

/// Shared across successive captures so readers cannot pin an earlier session above the cap.
#[derive(Debug, Default)]
pub(super) struct TraceMemory {
    bytes: AtomicUsize,
    records: AtomicUsize,
}

#[derive(Debug)]
struct RecordMemory {
    owner: Arc<TraceMemory>,
    bytes: usize,
}

impl Drop for RecordMemory {
    fn drop(&mut self) {
        self.owner.bytes.fetch_sub(self.bytes, Ordering::Relaxed);
        self.owner.records.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Retained-window metadata, separate from durable event replay or a snapshot cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceWindow {
    /// Oldest still-retained sequence, or none before the first accepted record.
    pub first_sequence: Option<u64>,
    /// Next process-local sequence, including attempted records dropped during formatting.
    pub next_sequence: u64,
    /// Number of records in the one service ring.
    pub retained_records: usize,
    /// Encoded bytes in the one service ring; reader-held entries remain separately accounted.
    pub retained_bytes: usize,
    /// Records evicted since this capture started.
    pub evicted_records: u64,
    /// Producer drops since this capture started, including contention without a sequence.
    pub dropped_records: u64,
    /// Retained submissions whose formatter was asked to shed optional detail.
    pub shed_records: u64,
}

/// At most one shared record and constant-sized window evidence per read.
#[derive(Debug)]
pub struct TraceRead {
    /// First retained record after the requested sequence, or none when caught up.
    pub record: Option<Arc<RetainedTrace>>,
    /// Metadata lets transports explicitly explain overflow, truncation, and producer drops.
    pub window: TraceWindow,
}

#[derive(Debug)]
pub(super) struct TraceRing {
    limits: RingLimits,
    records: VecDeque<Arc<RetainedTrace>>,
    bytes: usize,
    evicted: u64,
    shed: u64,
    memory: Arc<TraceMemory>,
}

impl TraceRing {
    pub(super) fn new(limits: RingLimits, memory: Arc<TraceMemory>) -> Self {
        Self {
            limits,
            records: VecDeque::new(),
            bytes: 0,
            evicted: 0,
            shed: 0,
            memory,
        }
    }

    pub(super) fn pressured(&self) -> bool {
        self.bytes >= self.limits.bytes.saturating_mul(3) / 4
            || self.records.len() >= self.limits.records.saturating_mul(3) / 4
    }

    pub(super) fn push(
        &mut self,
        sequence: u64,
        diagnostic: SanitizedDiagnostic,
        shed: bool,
        resources: &RuntimeResourceBudget,
    ) -> bool {
        let bytes = diagnostic.encoded_json().len();
        if bytes > MAX_TRACE_RECORD_BYTES || bytes > self.limits.bytes {
            return false;
        }
        while self.bytes.saturating_add(bytes) > self.limits.bytes
            || self.records.len() >= self.limits.records
        {
            let Some(record) = self.records.pop_front() else {
                return false;
            };
            self.bytes -= record.diagnostic.encoded_json().len();
            self.evicted = self.evicted.saturating_add(1);
        }
        // Submissions are serialized by the manager's producer try-lock; readers can only
        // release this accounting concurrently, so checking before addition cannot overbook.
        if self
            .memory
            .bytes
            .load(Ordering::Relaxed)
            .saturating_add(bytes)
            > self.limits.bytes
            || self.memory.records.load(Ordering::Relaxed) >= self.limits.records
        {
            return false;
        }
        let Some(reservation) = resources.try_reserve_diagnostic(WorkClass::CaptureTrace, bytes)
        else {
            return false;
        };
        self.memory.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.memory.records.fetch_add(1, Ordering::Relaxed);
        self.records.push_back(Arc::new(RetainedTrace {
            sequence,
            diagnostic,
            _reservation: reservation,
            _memory: RecordMemory {
                owner: Arc::clone(&self.memory),
                bytes,
            },
        }));
        self.bytes += bytes;
        self.shed = self.shed.saturating_add(u64::from(shed));
        true
    }

    pub(super) fn read_after(&self, after: Option<u64>, next: u64, dropped: u64) -> TraceRead {
        TraceRead {
            record: self
                .records
                .iter()
                .find(|record| after.is_none_or(|after| record.sequence > after))
                .cloned(),
            window: TraceWindow {
                first_sequence: self.records.front().map(|record| record.sequence),
                next_sequence: next,
                retained_records: self.records.len(),
                retained_bytes: self.bytes,
                evicted_records: self.evicted,
                dropped_records: dropped,
                shed_records: self.shed,
            },
        }
    }
}
