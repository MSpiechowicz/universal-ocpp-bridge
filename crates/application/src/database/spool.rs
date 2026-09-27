use std::{error::Error, fmt, future::Future, pin::Pin};

use uob_contracts::ExportDestination;

use crate::{
    BudgetedRecordChunk, CommittedRecordCursor, CommittedRecordDescriptor, CommittedRecordField,
    Durability, PageLimit,
};

/// Physical envelope for the optional local export database, not operational storage.
pub const EXPORT_SPOOL_ENVELOPE_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum size of one legacy v1 encoded row during migration.
pub const EXPORT_SPOOL_PAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSpoolNamespace {
    pub destination: ExportDestination,
    pub provider_kind: String,
    pub source_generation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSourceCheckpoint {
    pub cursor: CommittedRecordCursor,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportGapReason {
    SourceExpired,
    TelemetryDropped,
    TelemetryEvicted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportGap {
    pub durability: Durability,
    pub first: u64,
    pub last: u64,
    pub reason: ExportGapReason,
}

impl ExportGap {
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.last - self.first + 1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportPendingPosition {
    pub durability: Durability,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportPendingDescriptor {
    pub position: ExportPendingPosition,
    pub row_id: i64,
    pub record_id_len: u64,
    pub committed_at_len: u64,
    pub payload_len: u64,
    pub encoding_version: u32,
}

#[derive(Debug)]
pub struct ExportPendingPage {
    pub reservation: Option<crate::RuntimeReservation>,
    pub items: Vec<ExportPendingDescriptor>,
    pub resume: Option<ExportPendingPosition>,
    pub has_more: bool,
}

/// Recovered durable facts. Deferred means a known source high-water exceeds a checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSpoolStatus {
    pub destination: ExportDestination,
    pub provider_kind: String,
    pub source_generation: String,
    pub critical: Option<ExportSourceCheckpoint>,
    pub telemetry: Option<ExportSourceCheckpoint>,
    pub critical_high_water: u64,
    pub telemetry_high_water: u64,
    pub pending_records: u64,
    pub gaps: Vec<ExportGap>,
    pub incomplete: bool,
    pub legacy_baseline_incomplete: bool,
}

#[derive(Clone, Debug)]
pub struct ExportSpoolGapCommit {
    pub namespace: ExportSpoolNamespace,
    pub durability: Durability,
    pub expected: Option<ExportSourceCheckpoint>,
    pub next: ExportSourceCheckpoint,
    pub high_water: u64,
    pub gaps: Vec<ExportGap>,
    pub legacy_baseline_incomplete: bool,
}

#[derive(Clone, Debug)]
pub struct ExportSpoolRecordBegin {
    pub progress: ExportSpoolGapCommit,
    pub descriptor: CommittedRecordDescriptor,
}

pub enum ExportSpoolRecordAdmission {
    Transfer(Box<dyn ExportSpoolTransfer>),
    TelemetryDropped(ExportSpoolStatus),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportSpoolErrorCode {
    InvalidRequest,
    NamespaceConflict,
    CheckpointConflict,
    Backpressure,
    SummaryFull,
    Busy,
    Unavailable,
    IntegrityFailure,
    PendingExpired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSpoolError {
    code: ExportSpoolErrorCode,
    detail: &'static str,
}

impl ExportSpoolError {
    #[must_use]
    pub const fn new(code: ExportSpoolErrorCode, detail: &'static str) -> Self {
        Self { code, detail }
    }

    #[must_use]
    pub const fn code(&self) -> ExportSpoolErrorCode {
        self.code
    }
}

impl fmt::Display for ExportSpoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.detail)
    }
}

impl Error for ExportSpoolError {}

pub type ExportSpoolFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ExportSpoolError>> + Send + 'a>>;

/// Owns one provisional transaction; dropping this capability cancels it.
pub trait ExportSpoolTransfer: Send {
    fn append(&mut self, chunk: BudgetedRecordChunk) -> ExportSpoolFuture<'_, ()>;
    fn finish(self: Box<Self>) -> ExportSpoolFuture<'static, ExportSpoolStatus>;
    fn abort(self: Box<Self>) -> ExportSpoolFuture<'static, ()>;
}

/// Optional host-owned storage. No remote acknowledgement or delete capability.
pub trait ExportSpool: Send + Sync {
    fn status(&self, namespace: ExportSpoolNamespace) -> ExportSpoolFuture<'_, ExportSpoolStatus>;
    fn observe(
        &self,
        namespace: ExportSpoolNamespace,
        durability: Durability,
        high_water: u64,
        legacy_baseline_incomplete: bool,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus>;
    fn commit_gaps(
        &self,
        request: ExportSpoolGapCommit,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus>;
    fn begin_record(
        &self,
        request: ExportSpoolRecordBegin,
    ) -> ExportSpoolFuture<'_, ExportSpoolRecordAdmission>;
    fn pending(
        &self,
        namespace: ExportSpoolNamespace,
        after: Option<ExportPendingPosition>,
        limit: PageLimit,
    ) -> ExportSpoolFuture<'_, ExportPendingPage>;
    fn pending_chunk(
        &self,
        namespace: ExportSpoolNamespace,
        descriptor: ExportPendingDescriptor,
        field: CommittedRecordField,
        offset: u64,
        max_bytes: usize,
    ) -> ExportSpoolFuture<'_, BudgetedRecordChunk>;
}
