use std::{collections::BTreeSet, error::Error, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{
    AvailabilityState, CommandResult, ContractVersion, CorrelationId, DataPointValue, EventId,
    ResourceRef, RuntimeIdentity, TransactionSnapshot, TypedValue, UtcTimestamp,
};

macro_rules! export_text {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a value after rejecting empty or whitespace-only text.
            ///
            /// # Errors
            ///
            /// Returns [`ExportIdentityError`] when `value` is empty.
            pub fn new(value: impl Into<String>) -> Result<Self, ExportIdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(ExportIdentityError(stringify!($name)));
                }
                Ok(Self(value))
            }

            /// Returns the stable wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

export_text!(
    ExportRecordId,
    "Stable identity of one committed canonical export record."
);
export_text!(
    ExportBatchId,
    "Stable identity of one at-least-once export batch."
);
export_text!(
    ExportDestinationId,
    "Stable configured identity of an external export destination."
);
export_text!(
    ExportErrorCode,
    "Stable sanitized provider-independent export error code."
);

impl From<&EventId> for ExportRecordId {
    fn from(value: &EventId) -> Self {
        Self(value.as_str().to_owned())
    }
}

/// Failure to construct a stable export identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportIdentityError(&'static str);

impl fmt::Display for ExportIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cannot be empty", self.0)
    }
}

impl Error for ExportIdentityError {}

/// Deterministic child position within one committed parent record.
///
/// Producers assign an ordinal from the parent's canonical traversal order. The pair of parent
/// record ID and ordinal remains stable across retries without hashing mutable payload bytes.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct ExportSubrecordId(u32);

impl ExportSubrecordId {
    /// Creates a deterministic child identity from its canonical zero-based ordinal.
    #[must_use]
    pub const fn from_ordinal(ordinal: u32) -> Self {
        Self(ordinal)
    }

    /// Returns the canonical child ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.0
    }
}

/// Stable identity of a root record or one deterministically flattened child.
#[derive(
    Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct ExportRecordIdentity {
    /// Parent identity retained across every delivery attempt.
    pub record_id: ExportRecordId,
    /// Canonical child ordinal, absent for an unflattened root record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subrecord_id: Option<ExportSubrecordId>,
}

impl ExportRecordIdentity {
    /// Creates an unflattened root identity.
    #[must_use]
    pub const fn root(record_id: ExportRecordId) -> Self {
        Self {
            record_id,
            subrecord_id: None,
        }
    }

    /// Creates a child identity stable for the same parent and canonical ordinal.
    #[must_use]
    pub const fn child(record_id: ExportRecordId, ordinal: u32) -> Self {
        Self {
            record_id,
            subrecord_id: Some(ExportSubrecordId::from_ordinal(ordinal)),
        }
    }
}

/// Trusted metadata common to every privacy-filtered export record.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRecordMetadata {
    /// Stable root or deterministic child identity.
    pub identity: ExportRecordIdentity,
    /// Version of the export envelope and its closed payload surface.
    pub schema_version: ContractVersion,
    /// Trusted process and artifact context at observation time.
    pub runtime: RuntimeIdentity,
    /// Canonical resource independent of the external destination.
    pub resource: ResourceRef,
    /// Source timestamp, absent when the source supplied none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time: Option<UtcTimestamp>,
    /// UTC timestamp at which the bridge observed the record.
    pub observed_at: UtcTimestamp,
    /// Monotonic sequence in the owning resource stream.
    pub sequence: u64,
    /// Correlation identity shared with related records, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
}

/// Canonical point change without any target- or provider-specific fields.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportPointChange {
    /// Previous typed value, absent when no prior observation was available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_value: Option<TypedValue>,
    /// Current value with exact quantity, unit, phase, context, quality, and timestamps.
    pub current: DataPointValue,
}

/// Canonical charging-resource availability transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportResourceStatusChange {
    /// Previous observed state.
    pub previous: AvailabilityState,
    /// Current observed state.
    pub current: AvailabilityState,
}

/// Closed privacy-filtered payload surface accepted by external export buffering.
///
/// It deliberately contains no arbitrary JSON, credentials, authorization tokens, payment data,
/// or raw diagnostic payload variant. Producers must convert committed application state into one
/// of these canonical types before constructing an [`ExportRecord`].
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExportPayload {
    /// Canonical measurement preserving source semantics and exact precision.
    Measurement(DataPointValue),
    /// Canonical transaction lifecycle observation.
    TransactionLifecycle(TransactionSnapshot),
    /// Charging-resource availability transition.
    ResourceStatusChange(ExportResourceStatusChange),
    /// Canonical point-value transition.
    PointChange(ExportPointChange),
    /// Sanitized command lifecycle result and separately linked observed effects.
    CommandResult(CommandResult),
}

/// Stable export record category corresponding exactly to [`ExportPayload`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportRecordKind {
    /// Canonical measurement.
    Measurement,
    /// Transaction lifecycle observation.
    TransactionLifecycle,
    /// Charging-resource availability transition.
    ResourceStatusChange,
    /// Point-value transition.
    PointChange,
    /// Command lifecycle result.
    CommandResult,
}

impl ExportPayload {
    /// Returns the category encoded by this typed payload.
    #[must_use]
    pub const fn kind(&self) -> ExportRecordKind {
        match self {
            Self::Measurement(_) => ExportRecordKind::Measurement,
            Self::TransactionLifecycle(_) => ExportRecordKind::TransactionLifecycle,
            Self::ResourceStatusChange(_) => ExportRecordKind::ResourceStatusChange,
            Self::PointChange(_) => ExportRecordKind::PointChange,
            Self::CommandResult(_) => ExportRecordKind::CommandResult,
        }
    }
}

/// Privacy-filtered committed data eligible for an external provider batch.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRecord {
    metadata: ExportRecordMetadata,
    payload: ExportPayload,
}

impl ExportRecord {
    /// Creates a record only from the closed privacy-filtered payload surface.
    #[must_use]
    pub const fn new(metadata: ExportRecordMetadata, payload: ExportPayload) -> Self {
        Self { metadata, payload }
    }

    /// Returns trusted record metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ExportRecordMetadata {
        &self.metadata
    }

    /// Returns the privacy-filtered canonical payload.
    #[must_use]
    pub const fn payload(&self) -> &ExportPayload {
        &self.payload
    }

    /// Returns the record category without a separately mutable discriminator.
    #[must_use]
    pub const fn kind(&self) -> ExportRecordKind {
        self.payload.kind()
    }
}

/// Immutable external destination identity and configuration revision.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExportDestination {
    /// Stable configured provider instance identity.
    pub destination_id: ExportDestinationId,
    /// Immutable configuration revision to which this batch is bound.
    pub configuration_revision: u64,
}

/// A non-empty at-least-once batch bound to one immutable destination revision.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(try_from = "UncheckedExportBatch")]
pub struct ExportBatch {
    batch_id: ExportBatchId,
    destination: ExportDestination,
    records: Vec<ExportRecord>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UncheckedExportBatch {
    batch_id: ExportBatchId,
    destination: ExportDestination,
    records: Vec<ExportRecord>,
}

impl TryFrom<UncheckedExportBatch> for ExportBatch {
    type Error = ExportBatchError;

    fn try_from(value: UncheckedExportBatch) -> Result<Self, Self::Error> {
        Self::new(value.batch_id, value.destination, value.records)
    }
}

impl ExportBatch {
    /// Creates a non-empty batch and rejects repeated root/subrecord identities.
    ///
    /// # Errors
    ///
    /// Returns [`ExportBatchError`] when there are no records or an identity occurs twice.
    pub fn new(
        batch_id: ExportBatchId,
        destination: ExportDestination,
        records: Vec<ExportRecord>,
    ) -> Result<Self, ExportBatchError> {
        if records.is_empty() {
            return Err(ExportBatchError::Empty);
        }
        let mut identities = BTreeSet::new();
        for record in &records {
            let identity = &record.metadata().identity;
            if !identities.insert(identity) {
                return Err(ExportBatchError::DuplicateRecord(identity.clone()));
            }
        }
        Ok(Self {
            batch_id,
            destination,
            records,
        })
    }

    /// Returns the stable delivery-attempt identity.
    #[must_use]
    pub const fn batch_id(&self) -> &ExportBatchId {
        &self.batch_id
    }

    /// Returns the immutable destination binding.
    #[must_use]
    pub const fn destination(&self) -> &ExportDestination {
        &self.destination
    }

    /// Returns records in their canonical delivery order.
    #[must_use]
    pub fn records(&self) -> &[ExportRecord] {
        &self.records
    }
}

/// Invalid batch shape detected before it can reach an export provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportBatchError {
    /// A batch cannot carry zero records.
    Empty,
    /// The same root/subrecord identity occurred more than once.
    DuplicateRecord(ExportRecordIdentity),
}

impl fmt::Display for ExportBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("export batch cannot be empty"),
            Self::DuplicateRecord(identity) => write!(
                formatter,
                "duplicate export record {}:{:?}",
                identity.record_id, identity.subrecord_id
            ),
        }
    }
}

impl Error for ExportBatchError {}

mod report;

pub use report::{
    ExportOutcome, ExportRecordOutcome, ExportRecordOutcomeKind, ExportReport, ExportReportError,
    ExportUncertainStage,
};
