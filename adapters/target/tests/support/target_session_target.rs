use std::{future::poll_fn, sync::Arc};

use tokio::sync::mpsc;
use uob_application::{
    BridgeTarget, DeliveryId, DeliveryOutcome, DeliveryReport, ErrorRetryClassification,
    TargetContext, TargetDelivery, TargetDeliveryClass, TargetDescriptor, TargetError,
    TargetErrorCode, TargetMessage, TargetPortFuture, TargetReportPort, TargetTask,
};
use uob_contracts::{
    ArtifactDigest, AuthenticatedCommandOrigin, BridgeId, CommandOperation, CommandRequest,
    ContractVersion, Environment, EventOrigin, EventType, ExternalCommand, PrincipalId,
    ProcessInstanceId, ReleaseId, RequestId, ResourceRef, RuntimeIdentity, StationId,
    TargetInstanceId, UtcTimestamp,
};

pub(crate) enum PeerInput {
    Observation,
    Command(Box<ExternalCommand<()>>),
}

pub(crate) enum TargetBehavior {
    Run,
    FailRetryable,
    IgnoreShutdown,
}

pub(crate) fn fixture_target(
    descriptor: TargetDescriptor,
    peer: mpsc::Receiver<PeerInput>,
    observations: mpsc::Sender<String>,
    behavior: TargetBehavior,
) -> Box<dyn BridgeTarget<(), ()>> {
    Box::new(FixtureTarget {
        descriptor,
        peer,
        observations,
        behavior,
    })
}

struct FixtureTarget {
    descriptor: TargetDescriptor,
    peer: mpsc::Receiver<PeerInput>,
    observations: mpsc::Sender<String>,
    behavior: TargetBehavior,
}

impl BridgeTarget<(), ()> for FixtureTarget {
    fn descriptor(&self) -> TargetDescriptor {
        self.descriptor.clone()
    }

    fn run(mut self: Box<Self>, mut context: TargetContext<(), ()>) -> TargetTask {
        Box::pin(async move {
            match self.behavior {
                TargetBehavior::FailRetryable => {
                    return Err(TargetError::new(
                        TargetErrorCode::ConnectionUnavailable,
                        ErrorRetryClassification::Retryable,
                        "fixture.disconnected",
                    ));
                }
                TargetBehavior::IgnoreShutdown => std::future::pending::<()>().await,
                TargetBehavior::Run => {}
            }

            loop {
                tokio::select! {
                    delivery = poll_fn(|cx| context.deliveries.as_mut().poll_receive(cx)) => {
                        let Some(delivery) = delivery else { continue };
                        self.observations.send(delivery.delivery_id.as_str().to_owned())
                            .await.map_err(|_| fixture_error("fixture.observer_closed"))?;
                        let reports = Arc::clone(&context.critical_reports);
                        tokio::spawn(async move {
                            let _ = reports.report(DeliveryReport {
                                delivery_id: delivery.delivery_id,
                                outcome: DeliveryOutcome::LocallyExposed {
                                    surface: "fixture.surface".to_owned(),
                                },
                                reported_at: timestamp(),
                            }).await;
                        });
                    }
                    input = self.peer.recv() => {
                        let Some(input) = input else { continue };
                        match input {
                            PeerInput::Observation => {
                                self.observations.send("observation".to_owned()).await
                                    .map_err(|_| fixture_error("fixture.observer_closed"))?;
                            }
                            PeerInput::Command(command) => {
                                let code = context.commands.submit(*command).await
                                    .expect_err("fixture command port always rejects").code();
                                self.observations.send(format!("command:{code:?}")).await
                                    .map_err(|_| fixture_error("fixture.observer_closed"))?;
                            }
                        }
                    }
                    () = poll_fn(|cx| context.shutdown.as_mut().poll_shutdown(cx)) => return Ok(()),
                }
            }
        })
    }
}

fn fixture_error(context: &str) -> TargetError {
    TargetError::new(
        TargetErrorCode::ConnectionUnavailable,
        ErrorRetryClassification::Retryable,
        context,
    )
}

pub(crate) struct NoReports;

impl TargetReportPort for NoReports {
    fn report(&self, _report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub(crate) fn command(
    operation: CommandOperation<()>,
    origin: AuthenticatedCommandOrigin,
) -> ExternalCommand<()> {
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: RequestId::new("request").expect("request"),
            correlation_id: None,
            resource: station(),
            operation,
            expires_at: timestamp(),
        },
        origin,
    )
}

pub(crate) fn delivery(id: &str, target: &str, revision: u64) -> TargetDelivery<()> {
    TargetDelivery {
        delivery_id: DeliveryId::new(id).expect("delivery"),
        target_instance_id: target_id(target),
        target_configuration_revision: revision,
        station_ordering_key: station(),
        deadline: timestamp(),
        class: TargetDeliveryClass::Durable,
        message: Arc::new(TargetMessage::DomainEvent(uob_contracts::EventEnvelope {
            event_id: uob_contracts::EventId::new(format!("event-{id}")).expect("event"),
            schema_version: ContractVersion::V1_INITIAL,
            runtime: RuntimeIdentity {
                environment: Environment::Demo,
                release_id: ReleaseId::new("test-release").expect("release"),
                release_digest: ArtifactDigest::new("sha256:test").expect("digest"),
                process_instance_id: ProcessInstanceId::new("test-process").expect("process"),
            },
            resource: station(),
            source_time: None,
            observed_at: timestamp(),
            event_type: EventType::new("test.event").expect("event type"),
            origin: EventOrigin::Bridge,
            sequence: 1,
            correlation_id: None,
            causation_id: None,
            provenance: None,
            payload: (),
        })),
    }
}

fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-test").expect("bridge"),
        station_id: StationId::new("station-a").expect("station"),
        resource: None,
        native_protocol_reference: None,
    }
}

pub(crate) fn target_id(value: &str) -> TargetInstanceId {
    TargetInstanceId::new(value).expect("target")
}

pub(crate) fn principal() -> PrincipalId {
    PrincipalId::new("fixture-principal").expect("principal")
}

pub(crate) fn timestamp() -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1))
}
