use uob_application::{TargetDescriptor, TargetMessageClass};

/// Stable reasons a target descriptor does not satisfy the shared contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorViolation {
    /// No outbound canonical message class was declared.
    MissingOutboundSurface,
    /// An advertised outbound surface has no delivery meaning.
    MissingDeliverySemantic,
    /// The target accepts messages of unbounded size.
    UnboundedMessageSize,
    /// The target can receive deliveries but declares no delivery capacity.
    MissingDeliveryCapacity,
    /// The target advertises commands but declares no command capacity.
    MissingCommandCapacity,
    /// Diagnostic delivery was advertised without the optional tracing capability.
    DiagnosticsWithoutCapability,
}

/// Inspects the common, transport-independent invariants every target must advertise.
///
/// Returning every violation makes deliberately broken fixtures fail for the missing behavior
/// instead of stopping after an unrelated first error.
#[must_use]
pub fn inspect_descriptor(descriptor: &TargetDescriptor) -> Vec<DescriptorViolation> {
    let mut violations = Vec::new();
    if descriptor.outbound_message_classes.is_empty() {
        violations.push(DescriptorViolation::MissingOutboundSurface);
    }
    if !descriptor.outbound_message_classes.is_empty() && descriptor.delivery_semantics.is_empty() {
        violations.push(DescriptorViolation::MissingDeliverySemantic);
    }
    if descriptor.limits.maximum_message_bytes == 0 {
        violations.push(DescriptorViolation::UnboundedMessageSize);
    }
    if !descriptor.outbound_message_classes.is_empty()
        && descriptor.limits.maximum_in_flight_deliveries == 0
    {
        violations.push(DescriptorViolation::MissingDeliveryCapacity);
    }
    if !descriptor.inbound_operations.is_empty()
        && descriptor.limits.maximum_in_flight_commands == 0
    {
        violations.push(DescriptorViolation::MissingCommandCapacity);
    }
    if descriptor
        .outbound_message_classes
        .contains(&TargetMessageClass::Diagnostic)
        && !descriptor
            .optional_capabilities
            .iter()
            .any(|capability| capability.0 == "redacted-tracing")
    {
        violations.push(DescriptorViolation::DiagnosticsWithoutCapability);
    }
    violations
}
