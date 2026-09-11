use std::time::Duration;
use uob_application::{AdmissionError, RuntimeReservation};
use uob_contracts::{CorrelationId, ProtocolActionName, ProtocolEdition, StationId};

/// Correlation assigned by the authenticated connection and requesting workflow.
/// Request IDs must not be reused for this report kind within one connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportKey {
    pub station: StationId,
    pub connection: CorrelationId,
    pub protocol: ProtocolEdition,
    pub action: ProtocolActionName,
    pub request_id: u32,
    pub correlation: CorrelationId,
}

impl ReportKey {
    pub(super) fn valid(&self) -> bool {
        i32::try_from(self.request_id).is_ok()
            && [
                self.station.as_str(),
                self.connection.as_str(),
                self.action.as_str(),
                self.correlation.as_str(),
            ]
            .iter()
            .all(|value| value.len() <= 128)
    }
}

/// Per-report bounds, subordinate to the shared process budget.
#[derive(Clone, Copy, Debug)]
pub struct ReportLimits {
    pub maximum_bytes: usize,
    pub maximum_items: usize,
    pub maximum_fragments: u32,
    /// Absolute collection window; fragments never extend it.
    pub timeout: Duration,
}

impl Default for ReportLimits {
    fn default() -> Self {
        Self {
            maximum_bytes: 1024 * 1024,
            maximum_items: 4096,
            maximum_fragments: 256,
            timeout: Duration::from_secs(30),
        }
    }
}

impl ReportLimits {
    pub(super) fn valid(self) -> bool {
        (1..=8 * 1024 * 1024).contains(&self.maximum_bytes)
            && (1..=65_536).contains(&self.maximum_items)
            && (1..=4096).contains(&self.maximum_fragments)
            && !self.timeout.is_zero()
            && self.timeout <= Duration::from_secs(300)
    }
}

/// One already decoded and validated native report fragment.
/// The source must enforce the OCPP frame cap before decoding, retain its ingress
/// reservation until handoff, and route only this request's reports. Item bytes
/// preserve native fields; they are not diagnostic-safe or executable content.
pub struct ReportFragment {
    pub key: ReportKey,
    pub sequence: u32,
    pub more: bool,
    pub items: Vec<Vec<u8>>,
}

/// Payload-free reason why a report cannot be treated as complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportFailure {
    InvalidConfiguration,
    CorrelationMismatch,
    DuplicateOrConflictingSequence,
    MissingOrOutOfOrderSequence,
    FragmentLimit,
    ItemLimit,
    ByteLimit,
    Capacity(AdmissionError),
    TimedOut,
    Cancelled,
    Disconnected,
    InvalidFragment,
}

/// Counts cover only fully accepted fragments; rejected fragments are never retained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReportProgress {
    pub fragments: u32,
    pub items: usize,
    pub bytes: usize,
}

/// An incomplete/truncated report carries correlation and counts, never a success payload.
#[derive(Debug)]
pub struct PartialReport {
    pub key: ReportKey,
    pub progress: ReportProgress,
    pub reason: ReportFailure,
}

/// Complete ordered content. Its budget reservation lasts as long as the content.
/// Deliberately neither Clone nor serializable: copying requires separate admission.
pub struct CollectedReport {
    pub(super) key: ReportKey,
    pub(super) progress: ReportProgress,
    pub(super) items: Vec<Box<[u8]>>,
    pub(super) reservation: RuntimeReservation,
}

impl CollectedReport {
    #[must_use]
    pub const fn key(&self) -> &ReportKey {
        &self.key
    }

    #[must_use]
    pub const fn progress(&self) -> ReportProgress {
        self.progress
    }

    /// Borrow native items in fragment/within-fragment order, without copying buffers.
    pub fn items(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.items.iter().map(AsRef::as_ref)
    }
}
