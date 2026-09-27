use std::sync::Arc;

use crate::{RuntimeReservation, StorageError, StorageErrorCode};

use super::{CommittedRecordCursor, Durability};

/// Maximum source bytes returned by a single field read.
pub const EXPORT_RECORD_CHUNK_BYTES: usize = 64 * 1024;

/// Source-row locator. It is not a snapshot or a retention pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecordReadToken {
    generation: Arc<str>,
    durability: Durability,
    sequence: u64,
    row_id: i64,
    lengths: [u64; 3],
    authenticator: u64,
}

impl CommittedRecordReadToken {
    /// Constructs a locator signed by the operational adapter. External callers cannot
    /// manufacture a valid locator without that worker's private authentication key.
    #[must_use]
    pub fn new(
        generation: Arc<str>,
        durability: Durability,
        sequence: u64,
        row_id: i64,
        lengths: [u64; 3],
        authenticator: u64,
    ) -> Self {
        Self {
            generation,
            durability,
            sequence,
            row_id,
            lengths,
            authenticator,
        }
    }

    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    #[must_use]
    pub const fn durability(&self) -> Durability {
        self.durability
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn row_id(&self) -> i64 {
        self.row_id
    }

    #[must_use]
    pub const fn lengths(&self) -> [u64; 3] {
        self.lengths
    }

    #[must_use]
    pub const fn authenticator(&self) -> u64 {
        self.authenticator
    }
}

/// Raw UTF-8 source fields. `RecordId` is literal UTF-8; `CommittedAt` is the
/// stored JSON timestamp string and `Payload` the stored JSON serialization.
/// Chunk boundaries need not be UTF-8 boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommittedRecordField {
    RecordId,
    CommittedAt,
    Payload,
}

impl CommittedRecordField {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::RecordId => 0,
            Self::CommittedAt => 1,
            Self::Payload => 2,
        }
    }
}

/// Only metadata is exposed during source discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecordDescriptor {
    pub token: CommittedRecordReadToken,
    pub durability: Durability,
    pub sequence: u64,
    pub cursor: CommittedRecordCursor,
    pub record_id_len: u64,
    pub committed_at_len: u64,
    pub payload_len: u64,
}

/// Bounded source page with a live-tail checkpoint even when empty.
#[derive(Debug)]
pub struct CommittedRecordPage {
    pub items: Vec<CommittedRecordDescriptor>,
    pub source_generation: String,
    pub resume_cursor: CommittedRecordCursor,
    pub has_more: bool,
    pub high_water: u64,
    pub expired_prefix: u64,
    pub lost_records: u64,
    pub legacy_baseline_incomplete: bool,
    /// Retains metadata admission until the page is consumed.
    pub reservation: Option<RuntimeReservation>,
}

/// A checked byte range from one immutable field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecordChunkQuery {
    pub token: CommittedRecordReadToken,
    pub field: CommittedRecordField,
    pub offset: u64,
    pub max_bytes: usize,
}

/// A source record may expire between discovery and individual field reads.
#[derive(Debug)]
pub enum CommittedRecordChunkResult {
    Data(BudgetedRecordChunk),
    Expired,
}

/// Owns both chunk bytes and their optional-memory admission until the consumer drops them.
#[derive(Debug)]
pub struct BudgetedRecordChunk {
    pub field: CommittedRecordField,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub next_offset: u64,
    pub end_of_field: bool,
    reservation: RuntimeReservation,
}

impl BudgetedRecordChunk {
    /// Takes an already admitted buffer; a caller cannot construct a chunk without its guard.
    ///
    /// # Errors
    /// Rejects any inconsistent range or byte count.
    pub fn new(
        field: CommittedRecordField,
        offset: u64,
        bytes: Vec<u8>,
        field_len: u64,
        reservation: RuntimeReservation,
    ) -> Result<Self, StorageError> {
        let count = bytes.len();
        let next_offset = offset
            .checked_add(u64::try_from(count).map_err(|_| invalid_chunk())?)
            .ok_or_else(invalid_chunk)?;
        if count > EXPORT_RECORD_CHUNK_BYTES
            || count > reservation.bytes()
            || next_offset > field_len
        {
            return Err(invalid_chunk());
        }
        Ok(Self {
            field,
            offset,
            bytes,
            next_offset,
            end_of_field: next_offset == field_len,
            reservation,
        })
    }

    /// Checks a chunk again at its consumer boundary after public fields may have changed.
    #[must_use]
    pub fn is_valid_for(&self, field_len: u64) -> bool {
        let Some(next_offset) = u64::try_from(self.bytes.len())
            .ok()
            .and_then(|count| self.offset.checked_add(count))
        else {
            return false;
        };
        self.bytes.len() <= EXPORT_RECORD_CHUNK_BYTES
            && self.bytes.len() <= self.reservation.bytes()
            && self.next_offset == next_offset
            && next_offset <= field_len
            && self.end_of_field == (next_offset == field_len)
    }
}

fn invalid_chunk() -> StorageError {
    StorageError::new(
        StorageErrorCode::InvalidRequest,
        "invalid committed-record chunk",
    )
}
