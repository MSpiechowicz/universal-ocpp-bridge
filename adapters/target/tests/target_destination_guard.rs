use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationSchema, DeliveryId,
    Durability, PendingDelivery, TargetConfiguration, ValidatedTargetConfiguration,
};
use uob_contracts::{
    BridgeId, Environment, EventId, ResourceRef, StationId, TargetInstanceId, UtcTimestamp,
};
use uob_target_adapter::{
    AuditedTargetDisposition, BridgeTargetSelection, ConfiguredTarget, TargetBacklogEntry,
    TargetBacklogState, TargetChangeError, TargetDestination, TargetDisplayFamily,
    TargetDispositionAction, TargetRegistration, TargetRegistry,
};

#[derive(Default)]
struct FactoryCalls {
    validated: AtomicUsize,
    created: AtomicUsize,
}

struct FixtureFactory {
    calls: Arc<FactoryCalls>,
}

impl BridgeTargetFactory<String, String> for FixtureFactory {
    fn kind(&self) -> &'static str {
        "test.memory"
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema::default()
    }

    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
        self.calls.validated.fetch_add(1, Ordering::SeqCst);
        Ok(ValidatedTargetConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        _configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<String, String>>, ConfigurationError> {
        self.calls.created.fetch_add(1, Ordering::SeqCst);
        unreachable!("preview must not construct the target")
    }
}

fn registry(calls: Arc<FactoryCalls>) -> TargetRegistry<String, String> {
    let mut registry = TargetRegistry::new();
    registry
        .register(
            FixtureFactory { calls },
            TargetRegistration {
                display_family: TargetDisplayFamily {
                    id: "test".to_owned(),
                    display_name: "Test".to_owned(),
                },
                presets: vec![],
                capabilities: vec![],
                transport_policy: None,
            },
        )
        .expect("register fixture target");
    registry
}

fn target_id(value: &str) -> TargetInstanceId {
    TargetInstanceId::new(value).expect("target instance ID")
}

fn destination(id: &str, revision: u64) -> TargetDestination {
    TargetDestination {
        target_instance_id: target_id(id),
        configuration_revision: revision,
    }
}

fn selection(id: &str, revision: u64) -> BridgeTargetSelection {
    BridgeTargetSelection {
        bridge_id: BridgeId::new("bridge-test").expect("bridge ID"),
        environment: Environment::Demo,
        target_id: target_id(id),
        targets: vec![ConfiguredTarget {
            kind: "test.memory".to_owned(),
            enabled: true,
            configuration: TargetConfiguration::new(target_id(id), revision),
            transport_security: None,
        }],
    }
}

fn backlog(owner: TargetDestination, count: u64) -> TargetBacklogState {
    TargetBacklogState {
        entries: vec![TargetBacklogEntry {
            destination: owner,
            pending_critical_deliveries: count,
        }],
    }
}

fn disposition(owner: TargetDestination, audit: &str) -> AuditedTargetDisposition {
    AuditedTargetDisposition {
        audit_event_id: audit.to_owned(),
        destination: owner,
        action: TargetDispositionAction::Archive,
    }
}

#[test]
fn changed_destination_with_pending_critical_work_requires_exact_audit() {
    let calls = Arc::new(FactoryCalls::default());
    let registry = registry(Arc::clone(&calls));
    let previous = destination("old-target", 7);
    let pending = backlog(previous.clone(), 3);

    let blocked =
        registry.preview_change(selection("new-target", 1), Some(&previous), &pending, &[]);
    assert!(matches!(
        blocked,
        Err(TargetChangeError::PendingDestinationChange)
    ));

    let preview = registry
        .preview_change(
            selection("new-target", 1),
            Some(&previous),
            &pending,
            &[disposition(previous.clone(), "audit-target-archive-42")],
        )
        .expect("matching audit permits explicit archive");
    assert!(preview.restart_required());
    assert_eq!(preview.running_destination(), Some(&previous));
    assert_eq!(preview.destination(), destination("new-target", 1));
    assert_eq!(preview.pending_critical_deliveries(), 3);
    assert_eq!(
        preview.dispositions()[0].action,
        TargetDispositionAction::Archive
    );
    assert_eq!(calls.validated.load(Ordering::SeqCst), 2);
    assert_eq!(calls.created.load(Ordering::SeqCst), 0);
}

#[test]
fn audit_must_name_every_exact_old_instance_and_revision() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let old_revision = destination("target-main", 7);
    let other_owner = destination("legacy-target", 2);
    let pending = TargetBacklogState {
        entries: vec![
            TargetBacklogEntry {
                destination: old_revision.clone(),
                pending_critical_deliveries: 2,
            },
            TargetBacklogEntry {
                destination: other_owner.clone(),
                pending_critical_deliveries: 1,
            },
        ],
    };

    let incomplete = registry.preview_change(
        selection("target-main", 8),
        Some(&old_revision),
        &pending,
        &[disposition(old_revision.clone(), "audit-one-owner")],
    );
    assert!(matches!(
        incomplete,
        Err(TargetChangeError::PendingDestinationChange)
    ));

    let wrong_revision = registry.preview_change(
        selection("target-main", 8),
        Some(&old_revision),
        &pending,
        &[
            disposition(destination("target-main", 6), "audit-wrong-revision"),
            disposition(other_owner, "audit-other-owner"),
        ],
    );
    assert!(matches!(
        wrong_revision,
        Err(TargetChangeError::InvalidDispositionAudit)
    ));
}

#[test]
fn unchanged_destination_preserves_pending_work_without_disposition() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let current = destination("target-main", 7);
    let preview = registry
        .preview_change(
            selection("target-main", 7),
            Some(&current),
            &backlog(current.clone(), 4),
            &[],
        )
        .expect("same owner may continue its pending work after restart");

    assert!(!preview.restart_required());
    assert_eq!(preview.destination(), current);
    assert!(preview.dispositions().is_empty());
}

#[test]
fn old_payloads_cannot_be_dispatched_by_the_new_selection() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let selection = registry
        .validate(selection("target-main", 8))
        .expect("new target selection");
    let old = pending_delivery("target-main", 7);
    let current = pending_delivery("target-main", 8);

    assert_eq!(
        selection.validate_pending_delivery_destination(&old),
        Err(TargetChangeError::DeliveryDestinationMismatch)
    );
    selection
        .validate_pending_delivery_destination(&current)
        .expect("exact owner remains dispatchable");
}

#[test]
fn extraneous_disposition_and_duplicate_backlog_owner_fail_closed() {
    let registry = registry(Arc::new(FactoryCalls::default()));
    let current = destination("target-main", 7);
    let extra = registry.preview_change(
        selection("target-main", 7),
        Some(&current),
        &backlog(current.clone(), 1),
        &[disposition(current.clone(), "audit-not-needed")],
    );
    assert!(matches!(
        extra,
        Err(TargetChangeError::DispositionWithoutPendingChange)
    ));

    let duplicate = TargetBacklogState {
        entries: vec![
            TargetBacklogEntry {
                destination: current.clone(),
                pending_critical_deliveries: 1,
            },
            TargetBacklogEntry {
                destination: current,
                pending_critical_deliveries: 2,
            },
        ],
    };
    assert!(matches!(
        registry.preview_change(selection("new-target", 1), None, &duplicate, &[]),
        Err(TargetChangeError::InvalidBacklogState)
    ));
}

fn pending_delivery(id: &str, revision: u64) -> PendingDelivery<String> {
    PendingDelivery {
        delivery_id: DeliveryId::new(format!("delivery-{id}-{revision}")).expect("delivery ID"),
        event_id: EventId::new(format!("event-{id}-{revision}")).expect("event ID"),
        target_instance_id: target_id(id),
        target_configuration_revision: revision,
        ordering_key: ResourceRef {
            bridge_id: BridgeId::new("bridge-test").expect("bridge ID"),
            station_id: StationId::new("station-test").expect("station ID"),
            resource: None,
            native_protocol_reference: None,
        },
        deadline: UtcTimestamp::new(
            PrimitiveDateTime::new(
                Date::from_calendar_date(2026, Month::September, 1).expect("date"),
                Time::from_hms(12, 0, 0).expect("time"),
            )
            .assume_offset(UtcOffset::UTC),
        ),
        durability: Durability::Critical,
        payload: "canonical payload".to_owned(),
    }
}
