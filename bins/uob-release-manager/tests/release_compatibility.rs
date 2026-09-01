use std::collections::BTreeSet;

use uob_contracts::ArtifactDigest;
use uob_release_manager::{
    ArtifactCompatibilityManifest, ConfigurationProjectionEvidence, DatabaseContinuity,
    DurableRecordClass, EvidenceResult, FormatCompatibility, FormatSupport, FormatVersions,
    MigrationPolicy, NormalPromotionGate, PromotionBlock, RollbackCycleEvidence, SchemaRange,
    SchemaVersion, SecurityCompatibilityPolicy,
};

fn version(value: u32) -> SchemaVersion {
    SchemaVersion::new(value)
}

fn range(minimum: u32, maximum: u32) -> SchemaRange {
    SchemaRange::new(version(minimum), version(maximum)).expect("schema range")
}

fn formats(maximum: u32) -> FormatCompatibility {
    let support = FormatSupport {
        readable: range(1, maximum),
        writable: range(1, maximum),
    };
    FormatCompatibility {
        public_contract: support,
        configuration: support,
        operational_sqlite: support,
        external_database: support,
    }
}

fn digest(value: &str) -> ArtifactDigest {
    ArtifactDigest::new(format!("sha256:{value}")).expect("artifact digest")
}

fn manifest(value: &str, release_sequence: u64, maximum: u32) -> ArtifactCompatibilityManifest {
    ArtifactCompatibilityManifest {
        artifact_digest: digest(value),
        release_sequence,
        formats: formats(maximum),
        local_migration: MigrationPolicy::AdditiveExpand,
        external_migration: MigrationPolicy::AdditiveExpand,
    }
}

fn evidence() -> RollbackCycleEvidence {
    RollbackCycleEvidence {
        old_artifact: digest("old"),
        new_artifact: digest("new"),
        returned_artifact: digest("old"),
        resulting_versions: FormatVersions {
            public_contract: version(1),
            configuration: version(1),
            operational_sqlite: version(1),
            external_database: version(1),
        },
        preserved_record_classes: BTreeSet::from([
            DurableRecordClass::Transaction,
            DurableRecordClass::MeterData,
            DurableRecordClass::CommandId,
            DurableRecordClass::TargetDelivery,
            DurableRecordClass::ExportCheckpoint,
            DurableRecordClass::AuditRecord,
        ]),
        candidate_created_records: EvidenceResult::Passed,
        old_binary_read_records: EvidenceResult::Passed,
        old_binary_wrote_records: EvidenceResult::Passed,
        unknown_values_round_trip: EvidenceResult::Passed,
        operational_database: DatabaseContinuity::ReusedInPlace,
        external_database: DatabaseContinuity::ReusedInPlace,
        configuration_projection: ConfigurationProjectionEvidence {
            source_version: version(1),
            target_version: version(1),
            preserves_unknown_fields: true,
            mutates_durable_state: false,
        },
    }
}

fn policy() -> SecurityCompatibilityPolicy {
    SecurityCompatibilityPolicy {
        minimum_release_sequence: 10,
        revoked_artifacts: BTreeSet::new(),
    }
}

fn evaluate(
    previous: &ArtifactCompatibilityManifest,
    candidate: &ArtifactCompatibilityManifest,
    evidence: &RollbackCycleEvidence,
) -> Result<uob_release_manager::QualifiedCompatibility, PromotionBlock> {
    NormalPromotionGate::evaluate(&policy(), previous, candidate, evidence)
}

#[test]
fn qualifies_complete_old_new_old_evidence_without_replacing_databases() {
    let previous = manifest("old", 10, 1);
    let candidate = manifest("new", 11, 2);

    let qualified = evaluate(&previous, &candidate, &evidence()).expect("compatible promotion");

    assert_eq!(qualified.previous_artifact(), &digest("old"));
    assert_eq!(qualified.candidate_artifact(), &digest("new"));
    assert_eq!(
        qualified.resulting_versions().operational_sqlite,
        version(1)
    );

    let mut without_optional_export = evidence();
    without_optional_export.external_database = DatabaseContinuity::NotConfigured;
    evaluate(&previous, &candidate, &without_optional_export)
        .expect("optional external database can be disabled");
}

#[test]
fn requires_every_new_durable_record_to_survive_the_cycle() {
    let previous = manifest("old", 10, 1);
    let candidate = manifest("new", 11, 2);
    let required = [
        DurableRecordClass::Transaction,
        DurableRecordClass::MeterData,
        DurableRecordClass::CommandId,
        DurableRecordClass::TargetDelivery,
        DurableRecordClass::ExportCheckpoint,
        DurableRecordClass::AuditRecord,
    ];

    for missing in required {
        let mut incomplete = evidence();
        incomplete.preserved_record_classes.remove(&missing);
        assert_eq!(
            evaluate(&previous, &candidate, &incomplete),
            Err(PromotionBlock::MissingDurableRecordClass(missing))
        );
    }
}

#[test]
fn unknown_values_and_unsupported_post_upgrade_formats_block_promotion() {
    let previous = manifest("old", 10, 1);
    let candidate = manifest("new", 11, 2);
    let mut unsafe_value = evidence();
    unsafe_value.unknown_values_round_trip = EvidenceResult::Failed;
    assert_eq!(
        evaluate(&previous, &candidate, &unsafe_value),
        Err(PromotionBlock::UnsafeUnknownValueRoundTrip)
    );

    let mut incompatible_schema = evidence();
    incompatible_schema.resulting_versions.operational_sqlite = version(2);
    assert_eq!(
        evaluate(&previous, &candidate, &incompatible_schema),
        Err(PromotionBlock::SchemaNotReadableByBoth)
    );
}

#[test]
fn destructive_or_reverse_migrations_never_qualify() {
    let previous = manifest("old", 10, 1);
    let mut candidate = manifest("new", 11, 2);
    candidate.local_migration = MigrationPolicy::DestructiveContraction;
    assert_eq!(
        evaluate(&previous, &candidate, &evidence()),
        Err(PromotionBlock::DestructiveLocalMigration)
    );

    candidate.local_migration = MigrationPolicy::AdditiveExpand;
    candidate.external_migration = MigrationPolicy::ReverseOnRollback;
    assert_eq!(
        evaluate(&previous, &candidate, &evidence()),
        Err(PromotionBlock::DestructiveExternalMigration)
    );
}

#[test]
fn rollback_and_configuration_projection_cannot_restore_durable_state() {
    let previous = manifest("old", 10, 1);
    let candidate = manifest("new", 11, 2);

    let mut restored = evidence();
    restored.operational_database = DatabaseContinuity::SnapshotRestored;
    assert_eq!(
        evaluate(&previous, &candidate, &restored),
        Err(PromotionBlock::DatabaseSnapshotWasRestored)
    );

    let mut projection = evidence();
    projection.configuration_projection.mutates_durable_state = true;
    assert_eq!(
        evaluate(&previous, &candidate, &projection),
        Err(PromotionBlock::ConfigurationProjectionMutatesDurableState)
    );
}

#[test]
fn signed_floor_revocation_and_exact_artifact_identity_override_version_order() {
    let previous = manifest("old", 9, 1);
    let candidate = manifest("new", 99, 2);
    assert_eq!(
        evaluate(&previous, &candidate, &evidence()),
        Err(PromotionBlock::PreviousBelowSecurityFloor)
    );

    let previous = manifest("old", 10, 1);
    let mut revoked_policy = policy();
    revoked_policy.revoked_artifacts.insert(digest("old"));
    assert_eq!(
        NormalPromotionGate::evaluate(&revoked_policy, &previous, &candidate, &evidence()),
        Err(PromotionBlock::PreviousRevoked)
    );

    let mut stale = evidence();
    stale.new_artifact = digest("different-candidate");
    assert_eq!(
        evaluate(&previous, &candidate, &stale),
        Err(PromotionBlock::EvidenceArtifactMismatch)
    );
}
