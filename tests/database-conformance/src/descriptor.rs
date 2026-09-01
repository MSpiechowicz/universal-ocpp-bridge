use uob_application::{
    DatabaseAcknowledgementScope, DatabaseProviderDescriptor, DeduplicationCapability,
};

/// Initial host policy shared by every external database provider.
pub const MAXIMUM_RECORDS_PER_BATCH: usize = 100;
/// Initial encoded canonical batch ceiling shared by every provider.
pub const MAXIMUM_BATCH_BYTES: usize = 256 * 1024;
/// The first release schedules a single serial export worker.
pub const MAXIMUM_IN_FLIGHT_BATCHES: usize = 1;

/// Stable reasons a provider descriptor does not satisfy the common contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorViolation {
    /// No canonical schema version was declared.
    MissingSchemaSurface,
    /// No canonical record class was declared.
    MissingRecordSurface,
    /// At-least-once retries cannot be deduplicated by canonical identity.
    MissingStableDeduplication,
    /// A transport handoff was advertised as acknowledgement evidence.
    FalseCommitAcknowledgement,
    /// The provider accepts an empty or policy-exceeding record batch.
    InvalidRecordLimit,
    /// The provider accepts an empty or policy-exceeding encoded batch.
    InvalidByteLimit,
    /// The provider can exceed the serial host scheduling policy.
    InvalidConcurrencyLimit,
}

/// Inspects transport-independent invariants required by the shared conformance suite.
#[must_use]
pub fn inspect_descriptor(descriptor: &DatabaseProviderDescriptor) -> Vec<DescriptorViolation> {
    let mut violations = Vec::new();
    if descriptor.record_schema_versions.is_empty() {
        violations.push(DescriptorViolation::MissingSchemaSurface);
    }
    if descriptor.supported_record_classes.is_empty() {
        violations.push(DescriptorViolation::MissingRecordSurface);
    }
    if descriptor.deduplication != DeduplicationCapability::StableRecordIdentity {
        violations.push(DescriptorViolation::MissingStableDeduplication);
    }
    if matches!(
        descriptor.acknowledgement_scope,
        DatabaseAcknowledgementScope::TransportHandoff(_)
    ) {
        violations.push(DescriptorViolation::FalseCommitAcknowledgement);
    }
    if descriptor.limits.maximum_records_per_batch == 0
        || descriptor.limits.maximum_records_per_batch > MAXIMUM_RECORDS_PER_BATCH
    {
        violations.push(DescriptorViolation::InvalidRecordLimit);
    }
    if descriptor.limits.maximum_batch_bytes == 0
        || descriptor.limits.maximum_batch_bytes > MAXIMUM_BATCH_BYTES
    {
        violations.push(DescriptorViolation::InvalidByteLimit);
    }
    if descriptor.limits.maximum_in_flight_batches == 0
        || descriptor.limits.maximum_in_flight_batches > MAXIMUM_IN_FLIGHT_BATCHES
    {
        violations.push(DescriptorViolation::InvalidConcurrencyLimit);
    }
    violations
}
