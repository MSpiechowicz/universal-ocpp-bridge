use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use time::OffsetDateTime;
use uob_contracts::{ContractVersion, TargetInstanceId, TargetKind, UtcTimestamp};

use super::*;

struct EmptyDeliveries;

impl TargetDeliveryReceiver<()> for EmptyDeliveries {
    fn poll_receive(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<TargetDelivery<()>>> {
        Poll::Ready(None)
    }

    fn capacity(&self) -> usize {
        1
    }

    fn backlog(&self) -> usize {
        0
    }
}

struct NoQueries;

impl TargetQueryPort<()> for NoQueries {
    fn query(&self, _query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "test.no_queries",
            ))
        })
    }

    fn subscribe_retained_events(
        &self,
        _query: RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "test.no_event_subscriptions",
            ))
        })
    }
}

impl TargetCommandPort<()> for NoQueries {
    fn submit(&self, _command: ExternalCommand<()>) -> TargetPortFuture<'_, CommandResult> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "test.no_commands",
            ))
        })
    }
}

impl TargetReportPort for NoQueries {
    fn report(&self, _report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

impl TargetDiagnosticPort for NoQueries {
    fn try_emit(&self, _diagnostic: TargetDiagnostic) -> Result<(), DiagnosticDrop> {
        Err(DiagnosticDrop::Disabled)
    }
}

struct ImmediateShutdown;

impl TargetShutdown for ImmediateShutdown {
    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }
}

struct FakeTarget {
    runs: Arc<AtomicUsize>,
    instance_id: TargetInstanceId,
}

impl BridgeTarget<(), ()> for FakeTarget {
    fn descriptor(&self) -> TargetDescriptor {
        TargetDescriptor {
            kind: TargetKind::new("test.memory").expect("target kind"),
            instance_id: self.instance_id.clone(),
            contract_version: ContractVersion::V1_INITIAL,
            outbound_message_classes: vec![TargetMessageClass::StationSnapshot],
            inbound_operations: Vec::new(),
            limits: TargetLimits {
                maximum_message_bytes: 1024,
                maximum_in_flight_deliveries: 1,
                maximum_in_flight_commands: 0,
            },
            delivery_semantics: vec![DeliverySemantic::LocalExposure],
            optional_capabilities: Vec::new(),
        }
    }

    fn run(self: Box<Self>, _context: TargetContext<(), ()>) -> TargetTask {
        Box::pin(async move {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct FakeFactory {
    runtime_connections: Arc<AtomicUsize>,
    runs: Arc<AtomicUsize>,
}

impl BridgeTargetFactory<(), ()> for FakeFactory {
    fn kind(&self) -> &'static str {
        "test.memory"
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema {
            fields: vec![ConfigurationField {
                name: "credentials".to_owned(),
                kind: ConfigurationFieldKind::CredentialReference,
                required: true,
            }],
        }
    }

    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
        match configuration.setting("credentials") {
            Some(ConfigurationValue::CredentialReference(_)) => {
                Ok(ValidatedTargetConfiguration::new(configuration.clone()))
            }
            Some(_) => Err(ConfigurationError::field(
                ConfigurationErrorCode::InvalidField,
                "credentials",
            )),
            None => Err(ConfigurationError::field(
                ConfigurationErrorCode::MissingField,
                "credentials",
            )),
        }
    }

    fn create(
        &self,
        configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<(), ()>>, ConfigurationError> {
        Ok(Box::new(FakeTarget {
            runs: Arc::clone(&self.runs),
            instance_id: configuration.configuration().target_instance_id.clone(),
        }))
    }
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn poll_ready(task: &mut TargetTask) -> Poll<Result<(), TargetError>> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    task.as_mut().poll(&mut context)
}

fn target_context() -> TargetContext<(), ()> {
    let ports = Arc::new(NoQueries);
    TargetContext {
        deliveries: Box::pin(EmptyDeliveries),
        queries: ports.clone(),
        commands: ports.clone(),
        critical_reports: ports.clone(),
        diagnostics: ports,
        limits: TargetRuntimeLimits {
            maximum_in_flight_deliveries: 1,
            maximum_in_flight_commands: 1,
            maximum_command_bytes: 1024,
        },
        shutdown: Box::pin(ImmediateShutdown),
        shutdown_deadline: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
    }
}

#[test]
fn fake_target_is_validated_constructed_and_run_as_a_trait_object() {
    let runtime_connections = Arc::new(AtomicUsize::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let factory: Box<dyn BridgeTargetFactory<(), ()>> = Box::new(FakeFactory {
        runtime_connections: Arc::clone(&runtime_connections),
        runs: Arc::clone(&runs),
    });
    let configuration = TargetConfiguration::new(
        TargetInstanceId::new("target-main").expect("target instance"),
        7,
    )
    .with_setting(
        "credentials",
        ConfigurationValue::CredentialReference(
            CredentialReference::new("vault://targets/main").expect("credential reference"),
        ),
    );

    let validated = factory
        .validate(&configuration)
        .expect("valid configuration");
    assert_eq!(runtime_connections.load(Ordering::SeqCst), 0);
    let target: Box<dyn BridgeTarget<(), ()>> =
        factory.create(validated).expect("constructed target");
    assert_eq!(target.descriptor().instance_id.as_str(), "target-main");

    let mut task = target.run(target_context());
    assert_eq!(poll_ready(&mut task), Poll::Ready(Ok(())));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(runtime_connections.load(Ordering::SeqCst), 0);
}

#[test]
fn validation_schema_and_errors_never_retain_credential_contents() {
    let factory = FakeFactory {
        runtime_connections: Arc::new(AtomicUsize::new(0)),
        runs: Arc::new(AtomicUsize::new(0)),
    };
    let credential = "do-not-print-this-secret";
    let invalid = TargetConfiguration::new(
        TargetInstanceId::new("target-main").expect("target instance"),
        1,
    )
    .with_setting(
        "credentials",
        ConfigurationValue::Text(credential.to_owned()),
    );

    let error = match factory.validate(&invalid) {
        Ok(_) => panic!("wrong credential type must fail validation"),
        Err(error) => error,
    };
    let diagnostic = format!("{:?} {error:?} {error}", factory.configuration_schema());

    assert!(!diagnostic.contains(credential));
    assert_eq!(error.code(), ConfigurationErrorCode::InvalidField);
    assert_eq!(factory.runtime_connections.load(Ordering::SeqCst), 0);
}

#[test]
fn delivery_outcomes_preserve_distinct_acknowledgement_meanings() {
    let local = DeliveryOutcome::LocallyExposed {
        surface: "scada.point_model".to_owned(),
    };
    let acknowledged = DeliveryOutcome::Acknowledged {
        peer: "broker-primary".to_owned(),
        scope: AcknowledgementScope("peer.packet_received".to_owned()),
    };
    let retryable = DeliveryOutcome::RetryableFailure {
        reason: "connection_unavailable".to_owned(),
    };
    let permanent = DeliveryOutcome::PermanentFailure {
        reason: "mapping_unsupported".to_owned(),
    };
    let uncertain = DeliveryOutcome::Uncertain {
        reason: "peer_timeout_after_send".to_owned(),
    };

    assert!(matches!(local, DeliveryOutcome::LocallyExposed { .. }));
    assert!(matches!(acknowledged, DeliveryOutcome::Acknowledged { .. }));
    assert!(matches!(
        retryable,
        DeliveryOutcome::RetryableFailure { .. }
    ));
    assert!(matches!(
        permanent,
        DeliveryOutcome::PermanentFailure { .. }
    ));
    assert!(matches!(uncertain, DeliveryOutcome::Uncertain { .. }));
    assert_ne!(local, acknowledged);
    assert_ne!(retryable, permanent);
    assert_ne!(permanent, uncertain);
}
