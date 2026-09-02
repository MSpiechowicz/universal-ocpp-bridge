use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Notify, mpsc};
use uob_application::{
    BridgeTarget, BridgeTargetFactory, CommandAdmissionError, CommandAdmissionErrorCode,
    CommandAdmissionFuture, CommandAdmissionPort, ConfigurationError, ConfigurationSchema,
    DeliveryId, DeliveryReport, DeliverySemantic, DiagnosticDrop, ErrorRetryClassification,
    RuntimeResourceBudget, RuntimeResourceLimits, TargetConfiguration, TargetDescriptor,
    TargetDiagnostic, TargetDiagnosticPort, TargetLimits, TargetMessageClass, TargetPortError,
    TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryPort, TargetQueryResult,
    TargetReportPort, TargetRetainedEventStream, TargetRuntimeLimits, ValidatedTargetConfiguration,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, CommandOperation, ContractVersion, Environment,
    ExternalCommand, Operation, TargetInstanceId, TargetKind,
};
use uob_target_adapter::{
    BridgeTargetSelection, ConfiguredTarget, TargetDeliveryIngressError, TargetDisplayFamily,
    TargetRegistration, TargetRegistry, TargetSessionError, TargetSessionOptions,
    TargetSessionPorts, spawn_target_session,
};

#[path = "support/target_session_target.rs"]
mod target_support;

use target_support::{
    NoReports, PeerInput, TargetBehavior, command, delivery, fixture_target, principal, target_id,
    timestamp,
};

struct FixtureFactory {
    creates: Arc<AtomicUsize>,
    peers: Mutex<VecDeque<mpsc::Receiver<PeerInput>>>,
    observations: mpsc::Sender<String>,
    behaviors: Mutex<VecDeque<TargetBehavior>>,
}

impl BridgeTargetFactory<(), ()> for FixtureFactory {
    fn kind(&self) -> &'static str {
        "test.session"
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
        configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<(), ()>>, ConfigurationError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let peer = self
            .peers
            .lock()
            .expect("peer queue")
            .pop_front()
            .expect("peer");
        let behavior = self
            .behaviors
            .lock()
            .expect("behavior queue")
            .pop_front()
            .expect("behavior");
        Ok(fixture_target(
            descriptor(configuration.configuration().target_instance_id.clone()),
            peer,
            self.observations.clone(),
            behavior,
        ))
    }
}

fn descriptor(instance_id: TargetInstanceId) -> TargetDescriptor {
    TargetDescriptor {
        kind: TargetKind::new("test.session").expect("kind"),
        instance_id,
        contract_version: ContractVersion::V1_INITIAL,
        outbound_message_classes: vec![TargetMessageClass::DomainEvent],
        inbound_operations: vec![Operation::Start],
        limits: TargetLimits {
            maximum_message_bytes: 4096,
            maximum_in_flight_deliveries: 2,
            maximum_in_flight_commands: 1,
        },
        delivery_semantics: vec![DeliverySemantic::LocalExposure],
        optional_capabilities: vec![],
    }
}

struct NoQueries;

impl TargetQueryPort<()> for NoQueries {
    fn query(&self, _query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "test",
            ))
        })
    }

    fn subscribe_retained_events(
        &self,
        _query: uob_application::RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "test",
            ))
        })
    }
}

#[derive(Default)]
struct RejectingCommands(AtomicUsize);

impl CommandAdmissionPort<()> for RejectingCommands {
    fn submit(
        &self,
        _command: ExternalCommand<()>,
    ) -> CommandAdmissionFuture<'_, uob_contracts::CommandResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(CommandAdmissionError::new(
                CommandAdmissionErrorCode::Unavailable,
                "fixture.command_unavailable",
            ))
        })
    }
}

struct BlockingReports {
    entered: mpsc::Sender<DeliveryId>,
    release: Arc<Notify>,
}

impl TargetReportPort for BlockingReports {
    fn report(&self, report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        let entered = self.entered.clone();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            entered
                .send(report.delivery_id)
                .await
                .map_err(|_| TargetPortError::new(TargetPortErrorCode::Unavailable, "test"))?;
            release.notified().await;
            Ok(())
        })
    }
}

#[derive(Default)]
struct SaturatedDiagnostics(AtomicUsize);

impl TargetDiagnosticPort for SaturatedDiagnostics {
    fn try_emit(&self, _diagnostic: TargetDiagnostic) -> Result<(), DiagnosticDrop> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(DiagnosticDrop::Full)
    }
}

struct Fixture {
    selection: uob_target_adapter::ValidatedTargetSelection<(), ()>,
    peer_senders: Vec<mpsc::Sender<PeerInput>>,
    observations: mpsc::Receiver<String>,
    creates: Arc<AtomicUsize>,
}

fn fixture(behaviors: Vec<TargetBehavior>) -> Fixture {
    let creates = Arc::new(AtomicUsize::new(0));
    let (observation_tx, observation_rx) = mpsc::channel(16);
    let mut peer_senders = Vec::new();
    let mut peer_receivers = VecDeque::new();
    for _ in &behaviors {
        let (sender, receiver) = mpsc::channel(8);
        peer_senders.push(sender);
        peer_receivers.push_back(receiver);
    }
    let factory = FixtureFactory {
        creates: Arc::clone(&creates),
        peers: Mutex::new(peer_receivers),
        observations: observation_tx,
        behaviors: Mutex::new(behaviors.into()),
    };
    let target_id = target_id("target-main");
    let mut registry = TargetRegistry::new();
    registry
        .register(factory, registration())
        .expect("register");
    let selection = registry
        .validate(BridgeTargetSelection {
            bridge_id: BridgeId::new("bridge-test").expect("bridge"),
            environment: Environment::Demo,
            target_id: target_id.clone(),
            targets: vec![ConfiguredTarget {
                kind: "test.session".to_owned(),
                enabled: true,
                configuration: TargetConfiguration::new(target_id, 7),
                transport_security: None,
            }],
        })
        .expect("selection");
    Fixture {
        selection,
        peer_senders,
        observations: observation_rx,
        creates,
    }
}

fn registration() -> TargetRegistration {
    TargetRegistration {
        display_family: TargetDisplayFamily {
            id: "test".to_owned(),
            display_name: "Test".to_owned(),
        },
        presets: vec![],
        capabilities: vec![],
        transport_policy: None,
    }
}

fn ports(
    commands: Arc<RejectingCommands>,
    reports: Arc<dyn TargetReportPort>,
    diagnostics: Arc<SaturatedDiagnostics>,
) -> TargetSessionPorts<(), ()> {
    TargetSessionPorts {
        queries: Arc::new(NoQueries),
        commands,
        critical_reports: reports,
        diagnostics,
    }
}

fn options() -> TargetSessionOptions {
    TargetSessionOptions {
        delivery_capacity: 2,
        critical_report_capacity: 1,
        runtime_limits: TargetRuntimeLimits {
            maximum_in_flight_deliveries: 2,
            maximum_in_flight_commands: 1,
            maximum_command_bytes: 4096,
        },
        shutdown_deadline: timestamp(),
    }
}

fn budget() -> Arc<RuntimeResourceBudget> {
    Arc::new(RuntimeResourceBudget::new(RuntimeResourceLimits::default()).expect("budget"))
}

#[tokio::test]
async fn slow_reports_and_saturated_diagnostics_do_not_starve_peer_ingress() {
    let mut fixture = fixture(vec![TargetBehavior::Run]);
    let commands = Arc::new(RejectingCommands::default());
    let diagnostics = Arc::new(SaturatedDiagnostics::default());
    let (report_tx, mut report_rx) = mpsc::channel(2);
    let release = Arc::new(Notify::new());
    let reports = Arc::new(BlockingReports {
        entered: report_tx,
        release: Arc::clone(&release),
    });
    let shared_budget = budget();
    let (ingress, task) = spawn_target_session(
        &fixture.selection,
        ports(commands, reports, Arc::clone(&diagnostics)),
        Arc::clone(&shared_budget),
        options(),
    )
    .expect("session");

    ingress
        .try_deliver(delivery("delivery-1", "target-main", 7), 128)
        .expect("first");
    ingress
        .try_deliver(delivery("delivery-2", "target-main", 7), 128)
        .expect("second");
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("delivery-1")
    );
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("delivery-2")
    );
    assert_eq!(
        report_rx.recv().await.expect("blocked report").as_str(),
        "delivery-1"
    );

    fixture.peer_senders[0]
        .send(PeerInput::Observation)
        .await
        .expect("observation");
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("observation")
    );
    assert!(diagnostics.0.load(Ordering::SeqCst) > 0);
    release.notify_one();
    assert_eq!(
        report_rx
            .recv()
            .await
            .expect("second blocked report")
            .as_str(),
        "delivery-2"
    );
    release.notify_one();
    task.shutdown(Duration::from_secs(1))
        .await
        .expect("shutdown");
    drop(ingress);
    tokio::task::yield_now().await;
    assert_eq!(shared_budget.snapshot().queued_payload_bytes, 0);
}

#[tokio::test]
async fn command_origin_capability_and_concurrency_are_enforced_before_admission() {
    let mut fixture = fixture(vec![TargetBehavior::Run]);
    let commands = Arc::new(RejectingCommands::default());
    let diagnostics = Arc::new(SaturatedDiagnostics::default());
    let reports: Arc<dyn TargetReportPort> = Arc::new(NoReports);
    let (_ingress, task) = spawn_target_session(
        &fixture.selection,
        ports(Arc::clone(&commands), reports, diagnostics),
        budget(),
        options(),
    )
    .expect("session");

    fixture.peer_senders[0]
        .send(PeerInput::Command(Box::new(command(
            CommandOperation::Stop {
                transaction_id: uob_contracts::TransactionId::new("tx").expect("tx"),
            },
            AuthenticatedCommandOrigin::Target {
                target_instance_id: target_id("target-main"),
                principal_id: principal(),
            },
        ))))
        .await
        .expect("unsupported");
    fixture.peer_senders[0]
        .send(PeerInput::Command(Box::new(command(
            CommandOperation::Start {
                authorization_reference: None,
            },
            AuthenticatedCommandOrigin::Management {
                principal_id: principal(),
            },
        ))))
        .await
        .expect("forged origin");
    fixture.peer_senders[0]
        .send(PeerInput::Command(Box::new(command(
            CommandOperation::Start {
                authorization_reference: None,
            },
            AuthenticatedCommandOrigin::Target {
                target_instance_id: target_id("target-main"),
                principal_id: principal(),
            },
        ))))
        .await
        .expect("supported");
    fixture.peer_senders[0]
        .send(PeerInput::Command(Box::new(command(
            CommandOperation::Start {
                authorization_reference: Some("x".repeat(8_192)),
            },
            AuthenticatedCommandOrigin::Target {
                target_instance_id: target_id("target-main"),
                principal_id: principal(),
            },
        ))))
        .await
        .expect("oversized");

    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("command:Unsupported")
    );
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("command:Unauthorized")
    );
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("command:Unavailable")
    );
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("command:InvalidRequest")
    );
    assert_eq!(commands.0.load(Ordering::SeqCst), 1);
    task.shutdown(Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn restart_reconstructs_only_the_selection_and_accepts_only_its_pending_revision() {
    let mut fixture = fixture(vec![TargetBehavior::FailRetryable, TargetBehavior::Run]);
    let diagnostics = Arc::new(SaturatedDiagnostics::default());
    let first = spawn_target_session(
        &fixture.selection,
        ports(
            Arc::new(RejectingCommands::default()),
            Arc::new(NoReports),
            Arc::clone(&diagnostics),
        ),
        budget(),
        options(),
    )
    .expect("first session")
    .1
    .wait()
    .await;
    assert!(matches!(first, Err(TargetSessionError::Target(error))
        if error.retry_classification() == ErrorRetryClassification::Retryable));

    let (ingress, task) = spawn_target_session(
        &fixture.selection,
        ports(
            Arc::new(RejectingCommands::default()),
            Arc::new(NoReports),
            diagnostics,
        ),
        budget(),
        options(),
    )
    .expect("restarted session");
    assert!(matches!(
        ingress.try_deliver(delivery("old-revision", "target-main", 6), 64),
        Err(TargetDeliveryIngressError::DestinationMismatch(_))
    ));
    ingress
        .try_deliver(delivery("pending-current", "target-main", 7), 64)
        .expect("matching recovered work");
    assert_eq!(
        fixture.observations.recv().await.as_deref(),
        Some("pending-current")
    );
    assert_eq!(fixture.creates.load(Ordering::SeqCst), 2);
    task.shutdown(Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn missed_shutdown_deadline_aborts_the_target_and_closes_delivery_ingress() {
    let fixture = fixture(vec![TargetBehavior::IgnoreShutdown]);
    let (ingress, task) = spawn_target_session(
        &fixture.selection,
        ports(
            Arc::new(RejectingCommands::default()),
            Arc::new(NoReports),
            Arc::new(SaturatedDiagnostics::default()),
        ),
        budget(),
        options(),
    )
    .expect("session");
    assert!(matches!(
        task.shutdown(Duration::from_millis(20)).await,
        Err(TargetSessionError::ShutdownDeadlineExceeded)
    ));
    assert!(matches!(
        ingress.try_deliver(delivery("late", "target-main", 7), 16),
        Err(TargetDeliveryIngressError::Closed(_))
    ));
}
