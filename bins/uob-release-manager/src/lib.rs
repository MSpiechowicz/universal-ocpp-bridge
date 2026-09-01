#![doc = "Independent release qualification and activation policy."]

use std::{collections::BTreeSet, error::Error, fmt};

use uob_contracts::ArtifactDigest;

/// Monotonic version of one persisted or externally visible format.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// Creates a schema version.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Inclusive schema-version interval supported by one artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaRange {
    minimum: SchemaVersion,
    maximum: SchemaVersion,
}

impl SchemaRange {
    /// Creates a non-empty inclusive range.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the minimum is greater than the maximum.
    pub const fn new(
        minimum: SchemaVersion,
        maximum: SchemaVersion,
    ) -> Result<Self, ManifestError> {
        if minimum.0 > maximum.0 {
            return Err(ManifestError::InvertedSchemaRange);
        }
        Ok(Self { minimum, maximum })
    }

    /// Reports whether the version is inside this interval.
    #[must_use]
    pub const fn contains(self, version: SchemaVersion) -> bool {
        self.minimum.0 <= version.0 && version.0 <= self.maximum.0
    }
}

/// Invalid release compatibility metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestError {
    /// The lower schema bound exceeded the upper bound.
    InvertedSchemaRange,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("schema range minimum exceeds maximum")
    }
}

impl Error for ManifestError {}

/// Read and write support for one format surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatSupport {
    /// Versions this artifact can decode without losing required meaning.
    pub readable: SchemaRange,
    /// Versions this artifact can safely emit or update.
    pub writable: SchemaRange,
}

/// Versions of every format that will exist after promotion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatVersions {
    /// Canonical public contract version.
    pub public_contract: SchemaVersion,
    /// Runtime configuration version.
    pub configuration: SchemaVersion,
    /// Authoritative local `SQLite` schema and record version.
    pub operational_sqlite: SchemaVersion,
    /// Optional external database schema and record version.
    pub external_database: SchemaVersion,
}

/// Complete supported format surface of one immutable artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatCompatibility {
    /// Public HTTP, MQTT, event, and export contracts.
    pub public_contract: FormatSupport,
    /// Configuration documents and projections.
    pub configuration: FormatSupport,
    /// Authoritative `SQLite` state and pending work.
    pub operational_sqlite: FormatSupport,
    /// External database records and checkpoints.
    pub external_database: FormatSupport,
}

/// Permitted database migration shape during the rollback support window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationPolicy {
    /// New optional structures are added without removing or reinterpreting old ones.
    AdditiveExpand,
    /// Existing structures are removed or changed incompatibly.
    DestructiveContraction,
    /// Downgrade attempts to reverse database changes.
    ReverseOnRollback,
}

/// Signed compatibility claims attached to an immutable release artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactCompatibilityManifest {
    /// Content digest of the artifact to which these claims apply.
    pub artifact_digest: ArtifactDigest,
    /// Monotonic signed security/compatibility sequence.
    pub release_sequence: u64,
    /// Explicit read/write ranges for every format surface.
    pub formats: FormatCompatibility,
    /// Migration behavior for authoritative local state.
    pub local_migration: MigrationPolicy,
    /// Migration behavior for optional remote state.
    pub external_migration: MigrationPolicy,
}

/// Durable record classes that an actual downgrade cycle must exercise.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DurableRecordClass {
    /// Charging transaction state created after upgrade.
    Transaction,
    /// Exact metering data created after upgrade.
    MeterData,
    /// Durable request identity used for command deduplication.
    CommandId,
    /// Pending target outbox delivery.
    TargetDelivery,
    /// External exporter cursor or checkpoint.
    ExportCheckpoint,
    /// Release or operational audit record.
    AuditRecord,
}

impl DurableRecordClass {
    const REQUIRED: [Self; 6] = [
        Self::Transaction,
        Self::MeterData,
        Self::CommandId,
        Self::TargetDelivery,
        Self::ExportCheckpoint,
        Self::AuditRecord,
    ];
}

/// Evidence that configuration was projected without rewriting persisted state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationProjectionEvidence {
    /// Configuration schema read after promotion.
    pub source_version: SchemaVersion,
    /// Version presented to the previous artifact after rollback.
    pub target_version: SchemaVersion,
    /// Unknown fields were retained for a later re-upgrade.
    pub preserves_unknown_fields: bool,
    /// Projection attempted to restore or mutate a database.
    pub mutates_durable_state: bool,
}

/// Result of one mandatory qualification assertion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceResult {
    /// The qualification harness observed the required behavior.
    Passed,
    /// The behavior failed or evidence was absent.
    Failed,
}

/// How one database was handled during the artifact downgrade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseContinuity {
    /// The old binary reopened the same post-upgrade database in place.
    ReusedInPlace,
    /// No optional external database was configured during this cycle.
    NotConfigured,
    /// A different database replaced the post-upgrade database.
    Replaced,
    /// A pre-upgrade snapshot was restored.
    SnapshotRestored,
    /// Schema changes were reversed for the downgrade.
    ReverseMigrated,
}

/// Results from executing a real old-to-new-to-old qualification cycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackCycleEvidence {
    /// Artifact that ran before promotion.
    pub old_artifact: ArtifactDigest,
    /// Candidate artifact that created the new records.
    pub new_artifact: ArtifactDigest,
    /// Artifact actually restarted for the downgrade step.
    pub returned_artifact: ArtifactDigest,
    /// Format versions left by the candidate.
    pub resulting_versions: FormatVersions,
    /// Record classes created by the candidate and checked after downgrade.
    pub preserved_record_classes: BTreeSet<DurableRecordClass>,
    /// The candidate, rather than a fixture setup step, created the checked records.
    pub candidate_created_records: EvidenceResult,
    /// The previous binary decoded every checked record after downgrade.
    pub old_binary_read_records: EvidenceResult,
    /// The previous binary updated state successfully after downgrade.
    pub old_binary_wrote_records: EvidenceResult,
    /// New or unknown enum/format values round-tripped without reinterpretation.
    pub unknown_values_round_trip: EvidenceResult,
    /// Treatment of the upgraded authoritative `SQLite` database.
    pub operational_database: DatabaseContinuity,
    /// Treatment of the external database and its advanced checkpoints.
    pub external_database: DatabaseContinuity,
    /// Lossless configuration-projection evidence.
    pub configuration_projection: ConfigurationProjectionEvidence,
}

/// Signed policy constraints independent of semantic-version ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityCompatibilityPolicy {
    /// Oldest signed release sequence still eligible for activation.
    pub minimum_release_sequence: u64,
    /// Artifact digests forbidden from activation regardless of sequence.
    pub revoked_artifacts: BTreeSet<ArtifactDigest>,
}

/// Stable reason that normal reversible promotion was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromotionBlock {
    CandidateBelowSecurityFloor,
    PreviousBelowSecurityFloor,
    CandidateRevoked,
    PreviousRevoked,
    EvidenceArtifactMismatch,
    DestructiveLocalMigration,
    DestructiveExternalMigration,
    SchemaNotReadableByBoth,
    SchemaNotWritableByBoth,
    ConfigurationProjectionMismatch,
    LossyConfigurationProjection,
    ConfigurationProjectionMutatesDurableState,
    MissingDurableRecordClass(DurableRecordClass),
    CandidateDidNotCreateRecords,
    PreviousBinaryCouldNotReadRecords,
    PreviousBinaryCouldNotWriteRecords,
    UnsafeUnknownValueRoundTrip,
    OperationalDatabaseWasReplaced,
    ExternalDatabaseWasReplaced,
    DatabaseSnapshotWasRestored,
    DatabaseMigrationWasReversed,
}

impl fmt::Display for PromotionBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "normal promotion blocked: {self:?}")
    }
}

impl Error for PromotionBlock {}

/// Proof that a candidate passed the normal reversible-promotion gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QualifiedCompatibility {
    previous_artifact: ArtifactDigest,
    candidate_artifact: ArtifactDigest,
    resulting_versions: FormatVersions,
}

impl QualifiedCompatibility {
    /// Returns the exact previous-good artifact proven usable after downgrade.
    #[must_use]
    pub const fn previous_artifact(&self) -> &ArtifactDigest {
        &self.previous_artifact
    }

    /// Returns the exact candidate artifact covered by the evidence.
    #[must_use]
    pub const fn candidate_artifact(&self) -> &ArtifactDigest {
        &self.candidate_artifact
    }

    /// Returns the post-upgrade format versions both artifacts can read and write.
    #[must_use]
    pub const fn resulting_versions(&self) -> FormatVersions {
        self.resulting_versions
    }
}

/// Evaluates evidence without performing migrations, restores, or activation.
pub struct NormalPromotionGate;

impl NormalPromotionGate {
    /// Qualifies a candidate for normal reversible promotion.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionBlock`] unless every compatibility and security requirement is proven.
    pub fn evaluate(
        policy: &SecurityCompatibilityPolicy,
        previous: &ArtifactCompatibilityManifest,
        candidate: &ArtifactCompatibilityManifest,
        evidence: &RollbackCycleEvidence,
    ) -> Result<QualifiedCompatibility, PromotionBlock> {
        Self::check_security(policy, previous, candidate)?;
        if evidence.old_artifact != previous.artifact_digest
            || evidence.new_artifact != candidate.artifact_digest
            || evidence.returned_artifact != previous.artifact_digest
        {
            return Err(PromotionBlock::EvidenceArtifactMismatch);
        }
        if candidate.local_migration != MigrationPolicy::AdditiveExpand {
            return Err(PromotionBlock::DestructiveLocalMigration);
        }
        if candidate.external_migration != MigrationPolicy::AdditiveExpand {
            return Err(PromotionBlock::DestructiveExternalMigration);
        }
        Self::check_formats(previous, candidate, evidence.resulting_versions)?;
        Self::check_projection(previous, evidence)?;
        for record_class in DurableRecordClass::REQUIRED {
            if !evidence.preserved_record_classes.contains(&record_class) {
                return Err(PromotionBlock::MissingDurableRecordClass(record_class));
            }
        }
        Self::check_cycle(evidence)?;
        Ok(QualifiedCompatibility {
            previous_artifact: previous.artifact_digest.clone(),
            candidate_artifact: candidate.artifact_digest.clone(),
            resulting_versions: evidence.resulting_versions,
        })
    }

    fn check_security(
        policy: &SecurityCompatibilityPolicy,
        previous: &ArtifactCompatibilityManifest,
        candidate: &ArtifactCompatibilityManifest,
    ) -> Result<(), PromotionBlock> {
        if candidate.release_sequence < policy.minimum_release_sequence {
            return Err(PromotionBlock::CandidateBelowSecurityFloor);
        }
        if previous.release_sequence < policy.minimum_release_sequence {
            return Err(PromotionBlock::PreviousBelowSecurityFloor);
        }
        if policy
            .revoked_artifacts
            .contains(&candidate.artifact_digest)
        {
            return Err(PromotionBlock::CandidateRevoked);
        }
        if policy.revoked_artifacts.contains(&previous.artifact_digest) {
            return Err(PromotionBlock::PreviousRevoked);
        }
        Ok(())
    }

    fn check_formats(
        previous: &ArtifactCompatibilityManifest,
        candidate: &ArtifactCompatibilityManifest,
        versions: FormatVersions,
    ) -> Result<(), PromotionBlock> {
        let formats = [
            (
                previous.formats.public_contract,
                candidate.formats.public_contract,
                versions.public_contract,
            ),
            (
                previous.formats.configuration,
                candidate.formats.configuration,
                versions.configuration,
            ),
            (
                previous.formats.operational_sqlite,
                candidate.formats.operational_sqlite,
                versions.operational_sqlite,
            ),
            (
                previous.formats.external_database,
                candidate.formats.external_database,
                versions.external_database,
            ),
        ];
        for (old, new, version) in formats {
            if !old.readable.contains(version) || !new.readable.contains(version) {
                return Err(PromotionBlock::SchemaNotReadableByBoth);
            }
            if !old.writable.contains(version) || !new.writable.contains(version) {
                return Err(PromotionBlock::SchemaNotWritableByBoth);
            }
        }
        Ok(())
    }

    fn check_projection(
        previous: &ArtifactCompatibilityManifest,
        evidence: &RollbackCycleEvidence,
    ) -> Result<(), PromotionBlock> {
        let projection = evidence.configuration_projection;
        if projection.source_version != evidence.resulting_versions.configuration
            || !previous
                .formats
                .configuration
                .readable
                .contains(projection.target_version)
        {
            return Err(PromotionBlock::ConfigurationProjectionMismatch);
        }
        if !projection.preserves_unknown_fields {
            return Err(PromotionBlock::LossyConfigurationProjection);
        }
        if projection.mutates_durable_state {
            return Err(PromotionBlock::ConfigurationProjectionMutatesDurableState);
        }
        Ok(())
    }

    fn check_cycle(evidence: &RollbackCycleEvidence) -> Result<(), PromotionBlock> {
        let checks = [
            (
                evidence.candidate_created_records,
                PromotionBlock::CandidateDidNotCreateRecords,
            ),
            (
                evidence.old_binary_read_records,
                PromotionBlock::PreviousBinaryCouldNotReadRecords,
            ),
            (
                evidence.old_binary_wrote_records,
                PromotionBlock::PreviousBinaryCouldNotWriteRecords,
            ),
            (
                evidence.unknown_values_round_trip,
                PromotionBlock::UnsafeUnknownValueRoundTrip,
            ),
        ];
        for (passed, error) in checks {
            if passed != EvidenceResult::Passed {
                return Err(error);
            }
        }
        Self::check_database_continuity(evidence.operational_database, true)?;
        Self::check_database_continuity(evidence.external_database, false)?;
        Ok(())
    }

    fn check_database_continuity(
        continuity: DatabaseContinuity,
        operational: bool,
    ) -> Result<(), PromotionBlock> {
        match continuity {
            DatabaseContinuity::ReusedInPlace => Ok(()),
            DatabaseContinuity::NotConfigured if !operational => Ok(()),
            DatabaseContinuity::NotConfigured => {
                Err(PromotionBlock::OperationalDatabaseWasReplaced)
            }
            DatabaseContinuity::Replaced if operational => {
                Err(PromotionBlock::OperationalDatabaseWasReplaced)
            }
            DatabaseContinuity::Replaced => Err(PromotionBlock::ExternalDatabaseWasReplaced),
            DatabaseContinuity::SnapshotRestored => {
                Err(PromotionBlock::DatabaseSnapshotWasRestored)
            }
            DatabaseContinuity::ReverseMigrated => {
                Err(PromotionBlock::DatabaseMigrationWasReversed)
            }
        }
    }
}
