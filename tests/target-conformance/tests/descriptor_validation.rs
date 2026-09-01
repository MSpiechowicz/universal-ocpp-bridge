use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use time::OffsetDateTime;
use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationSchema, DeliverySemantic,
    TargetConfiguration, TargetDescriptor, TargetLimits, TargetMessageClass, TargetRuntimeLimits,
    ValidatedTargetConfiguration,
};
use uob_contracts::{ContractVersion, Operation, TargetInstanceId, TargetKind, UtcTimestamp};
use uob_target_conformance::{
    DescriptorViolation, FakeTargetHost, HostCapacities, HostError, UnsupportedQueryPort,
    inspect_descriptor,
};

#[test]
fn broken_target_is_rejected_for_each_missing_advertised_behavior() {
    let broken = TargetDescriptor {
        kind: text(TargetKind::new, "test.broken"),
        instance_id: text(TargetInstanceId::new, "broken"),
        contract_version: ContractVersion::V1_INITIAL,
        outbound_message_classes: vec![TargetMessageClass::Diagnostic],
        inbound_operations: vec![Operation::Start],
        limits: TargetLimits {
            maximum_message_bytes: 0,
            maximum_in_flight_deliveries: 0,
            maximum_in_flight_commands: 0,
        },
        delivery_semantics: Vec::<DeliverySemantic>::new(),
        optional_capabilities: vec![],
    };
    let violations = inspect_descriptor(&broken);
    assert!(violations.contains(&DescriptorViolation::MissingDeliverySemantic));
    assert!(violations.contains(&DescriptorViolation::UnboundedMessageSize));
    assert!(violations.contains(&DescriptorViolation::MissingDeliveryCapacity));
    assert!(violations.contains(&DescriptorViolation::MissingCommandCapacity));
    assert!(violations.contains(&DescriptorViolation::DiagnosticsWithoutCapability));
}

#[test]
fn validation_is_network_free_and_required_host_channels_are_bounded() {
    let connections = Arc::new(AtomicUsize::new(0));
    let factory = OfflineFactory(Arc::clone(&connections));
    let configuration = TargetConfiguration::new(text(TargetInstanceId::new, "target-main"), 1);
    factory
        .validate(&configuration)
        .expect("offline validation");
    assert_eq!(connections.load(Ordering::SeqCst), 0);

    let invalid = FakeTargetHost::<(), ()>::build(
        HostCapacities {
            deliveries: 0,
            commands: 1,
            reports: 1,
            diagnostics: 0,
        },
        Arc::new(UnsupportedQueryPort),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 1,
            maximum_in_flight_commands: 1,
            maximum_command_bytes: 1,
        },
        UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
    );
    assert!(matches!(invalid, Err(HostError::InvalidCapacity)));
}

struct OfflineFactory(Arc<AtomicUsize>);

impl BridgeTargetFactory<(), ()> for OfflineFactory {
    fn kind(&self) -> &'static str {
        "test.offline"
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema::default()
    }

    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
        Ok(ValidatedTargetConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        _configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<(), ()>>, ConfigurationError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ConfigurationError::new(
            uob_application::ConfigurationErrorCode::Unsupported,
        ))
    }
}

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid fixture text")
}
