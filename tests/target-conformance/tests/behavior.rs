use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{Arc, Mutex},
};

use time::OffsetDateTime;
use tokio::{sync::mpsc, time::Duration};
use uob_application::{
    AcknowledgementScope, BridgeTarget, DeliveryOutcome, DeliveryReport, DeliverySemantic,
    ScopedTargetQueryPort, TargetContext, TargetDelivery, TargetDeliveryClass, TargetDescriptor,
    TargetDiagnostic, TargetHealth, TargetHealthState, TargetLimits, TargetMessage,
    TargetMessageClass, TargetPortError, TargetPortErrorCode, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryResult, TargetResourceScope,
    TargetRuntimeLimits, TargetTask,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, CommandLifecycle, CommandOperation, CommandRequest,
    CommandResult, CommandReturnRoute, ContractVersion, ExternalCommand, Operation, PrincipalId,
    RequestId, ResourceRef, StationId, TargetInstanceId, TargetKind, UtcTimestamp,
};
use uob_target_conformance::{
    DeliveryRecoveryLedger, FakeTargetHost, HostCapacities, RecoveryDisposition,
    UnsupportedQueryPort, inspect_descriptor,
};

#[path = "support/query.rs"]
mod query_support;
use query_support::CountingQuerySource;
enum PeerIngress {
    Command {
        request: CommandRequest<()>,
        claimed_principal: String,
        route_name: String,
    },
    Observation,
    Query(TargetQuery),
}
enum PeerResponse {
    Command(Result<CommandResult, TargetPortError>),
    Query(Result<TargetQueryResult<()>, TargetPortError>),
}
struct FakePeer {
    ingress: mpsc::Sender<PeerIngress>,
    responses: mpsc::Receiver<PeerResponse>,
    outcomes: Arc<Mutex<VecDeque<DeliveryOutcome>>>,
}

struct ReferenceTarget {
    descriptor: TargetDescriptor,
    ingress: mpsc::Receiver<PeerIngress>,
    responses: mpsc::Sender<PeerResponse>,
    outcomes: Arc<Mutex<VecDeque<DeliveryOutcome>>>,
    trusted_principal: PrincipalId,
    read_only: bool,
}

impl BridgeTarget<(), ()> for ReferenceTarget {
    fn descriptor(&self) -> TargetDescriptor {
        self.descriptor.clone()
    }

    fn run(mut self: Box<Self>, mut context: TargetContext<(), ()>) -> TargetTask {
        Box::pin(async move {
            let _ = context
                .diagnostics
                .try_emit(TargetDiagnostic::Health(TargetHealth {
                    state: TargetHealthState::Ready,
                    delivery_backlog: context.deliveries.backlog(),
                    in_flight_deliveries: 0,
                    active_connections: 1,
                    reason: Some("target.ready".to_owned()),
                }));
            loop {
                tokio::select! {
                    delivery = poll_fn(|cx| context.deliveries.as_mut().poll_receive(cx)) => {
                        let Some(delivery) = delivery else { continue };
                        let outcome = self.outcomes.lock().expect("outcomes").pop_front()
                            .unwrap_or_else(|| DeliveryOutcome::RetryableFailure {
                                reason: "peer.egress_stalled".to_owned(),
                            });
                        let reports = Arc::clone(&context.critical_reports);
                        tokio::spawn(async move {
                            let _ = reports.report(DeliveryReport {
                                delivery_id: delivery.delivery_id,
                                outcome,
                                reported_at: timestamp(1),
                            }).await;
                        });
                    }
                    ingress = self.ingress.recv() => {
                        let Some(ingress) = ingress else { continue };
                        handle_ingress(
                            ingress,
                            Arc::clone(&context.commands),
                            Arc::clone(&context.queries),
                            Arc::clone(&context.diagnostics),
                            self.responses.clone(),
                            self.descriptor.instance_id.clone(),
                            self.trusted_principal.clone(),
                            self.read_only,
                        ).await;
                    }
                    () = poll_fn(|cx| context.shutdown.as_mut().poll_shutdown(cx)) => break,
                }
            }
            Ok(())
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_ingress(
    ingress: PeerIngress,
    commands: Arc<dyn uob_application::TargetCommandPort<()>>,
    queries: Arc<dyn uob_application::TargetQueryPort<()>>,
    diagnostics: Arc<dyn uob_application::TargetDiagnosticPort>,
    responses: mpsc::Sender<PeerResponse>,
    target_instance_id: TargetInstanceId,
    trusted_principal: PrincipalId,
    read_only: bool,
) {
    match ingress {
        PeerIngress::Command {
            request,
            claimed_principal,
            route_name,
        } => {
            if claimed_principal.is_empty() || route_name.is_empty() {
                let _ = responses
                    .send(PeerResponse::Command(Err(TargetPortError::new(
                        TargetPortErrorCode::InvalidRequest,
                        "peer.metadata_missing",
                    ))))
                    .await;
                return;
            }
            let _ = diagnostics.try_emit(TargetDiagnostic::Health(TargetHealth {
                state: TargetHealthState::Ready,
                delivery_backlog: 0,
                in_flight_deliveries: 0,
                active_connections: 1,
                reason: Some("target.command_received".to_owned()),
            }));
            if request.expires_at <= timestamp(1) {
                let _ = responses
                    .send(PeerResponse::Command(Err(TargetPortError::new(
                        TargetPortErrorCode::Expired,
                        "command.expired",
                    ))))
                    .await;
                return;
            }
            let result = if read_only {
                Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "command.target_read_only",
                ))
            } else {
                commands
                    .submit(ExternalCommand::authenticated(
                        request,
                        AuthenticatedCommandOrigin::Target {
                            target_instance_id,
                            principal_id: trusted_principal,
                        },
                    ))
                    .await
            };
            let _ = responses.send(PeerResponse::Command(result)).await;
        }
        PeerIngress::Observation => {}
        PeerIngress::Query(query) => {
            let result = queries.query(query).await;
            let _ = responses.send(PeerResponse::Query(result)).await;
        }
    }
}

fn reference_target(
    read_only: bool,
    queries: Arc<dyn uob_application::TargetQueryPort<()>>,
) -> (
    Box<dyn BridgeTarget<(), ()>>,
    FakePeer,
    uob_target_conformance::HostContext<(), ()>,
) {
    let (ingress_tx, ingress_rx) = mpsc::channel(4);
    let (response_tx, response_rx) = mpsc::channel(4);
    let outcomes = Arc::new(Mutex::new(VecDeque::new()));
    let descriptor = descriptor(read_only);
    let target = ReferenceTarget {
        descriptor,
        ingress: ingress_rx,
        responses: response_tx,
        outcomes: Arc::clone(&outcomes),
        trusted_principal: text(PrincipalId::new, "configured-reader"),
        read_only,
    };
    let host = FakeTargetHost::build(
        HostCapacities {
            deliveries: 4,
            commands: 2,
            reports: 1,
            diagnostics: 1,
        },
        queries,
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 2,
            maximum_in_flight_commands: usize::from(!read_only),
            maximum_command_bytes: 1024,
        },
        timestamp(10),
    )
    .expect("bounded fake host");
    (
        Box::new(target),
        FakePeer {
            ingress: ingress_tx,
            responses: response_rx,
            outcomes,
        },
        host,
    )
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn capable_target_passes_bounded_bidirectional_and_recovery_scenarios() {
    let (target, mut peer, mut fixture) = reference_target(false, Arc::new(UnsupportedQueryPort));
    assert!(inspect_descriptor(&target.descriptor()).is_empty());
    let task = tokio::spawn(target.run(fixture.context));

    let ready = timeout(fixture.host.next_diagnostic())
        .await
        .expect("health");
    assert!(
        matches!(ready, TargetDiagnostic::Health(TargetHealth { reason: Some(reason), .. }) if reason == "target.ready")
    );

    peer.ingress
        .send(PeerIngress::Command {
            request: request("request-1", timestamp(5)),
            claimed_principal: "admin-from-payload".to_owned(),
            route_name: "trusted/admin/start".to_owned(),
        })
        .await
        .expect("peer command");
    let submitted = timeout(fixture.host.next_command())
        .await
        .expect("open command channel");
    assert!(matches!(
        &submitted.command.origin,
        AuthenticatedCommandOrigin::Target { principal_id, .. }
            if principal_id.as_str() == "configured-reader"
    ));
    let result = result_for(&submitted.command);
    submitted.respond(Ok(result.clone())).expect("respond");
    assert!(matches!(
        timeout(peer.responses.recv()).await.expect("peer response"),
        PeerResponse::Command(Ok(actual)) if actual == result
    ));
    let diagnostic = timeout(fixture.host.next_diagnostic())
        .await
        .expect("sanitized command diagnostic");
    let diagnostic = format!("{diagnostic:?}");
    assert!(!diagnostic.contains("admin-from-payload"));
    assert!(!diagnostic.contains("trusted/admin/start"));

    peer.ingress
        .send(PeerIngress::Command {
            request: request("expired", timestamp(0)),
            claimed_principal: "admin".to_owned(),
            route_name: "admin/start".to_owned(),
        })
        .await
        .expect("expired command");
    assert!(matches!(
        timeout(peer.responses.recv()).await.expect("expired response"),
        PeerResponse::Command(Err(error)) if error.code() == TargetPortErrorCode::Expired
    ));

    peer.ingress
        .send(PeerIngress::Observation)
        .await
        .expect("observation");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), fixture.host.next_command())
            .await
            .is_err()
    );

    peer.outcomes.lock().expect("outcomes").extend([
        DeliveryOutcome::Acknowledged {
            peer: "peer-a".to_owned(),
            scope: AcknowledgementScope("peer.packet_received".to_owned()),
        },
        DeliveryOutcome::RetryableFailure {
            reason: "connection_unavailable".to_owned(),
        },
        DeliveryOutcome::Uncertain {
            reason: "timeout_after_send".to_owned(),
        },
    ]);
    let deliveries = [delivery(1), delivery(2), delivery(3)];
    let mut ledger = DeliveryRecoveryLedger::default();
    for delivery in &deliveries {
        ledger.record(delivery.clone());
        fixture
            .host
            .try_deliver(delivery.clone())
            .expect("bounded delivery");
    }

    peer.ingress
        .send(PeerIngress::Command {
            request: request("request-2", timestamp(5)),
            claimed_principal: "owner".to_owned(),
            route_name: "admin/override".to_owned(),
        })
        .await
        .expect("concurrent command");
    let concurrent = timeout(fixture.host.next_command())
        .await
        .expect("ingress remains live while report egress stalls");
    let concurrent_result = result_for(&concurrent.command);
    concurrent
        .respond(Ok(concurrent_result))
        .expect("respond concurrent");

    let first = timeout(fixture.host.next_report()).await.expect("ack");
    let second = timeout(fixture.host.next_report()).await.expect("retry");
    let third = timeout(fixture.host.next_report())
        .await
        .expect("uncertain");
    assert_eq!(ledger.apply(&first), Some(RecoveryDisposition::Complete));
    assert_eq!(ledger.apply(&second), Some(RecoveryDisposition::Retryable));
    assert_eq!(ledger.apply(&third), Some(RecoveryDisposition::Reconcile));
    assert_eq!(ledger.pending().count(), 2);

    fixture.host.request_shutdown();
    timeout(task).await.expect("join").expect("target");
}

#[tokio::test]
async fn read_only_target_rejects_commands_and_scoped_queries_explicitly() {
    let source = Arc::new(CountingQuerySource::default());
    let allowed = station("station-a");
    let query_port = Arc::new(ScopedTargetQueryPort::new(
        source.clone(),
        TargetQueryAuthorization::new(
            text(TargetInstanceId::new, "target-main"),
            vec![TargetQueryPermission::StationSnapshots],
            vec![TargetResourceScope::Resource(allowed)],
        ),
    ));
    let (target, mut peer, mut fixture) = reference_target(true, query_port);
    assert!(inspect_descriptor(&target.descriptor()).is_empty());
    let task = tokio::spawn(target.run(fixture.context));

    peer.ingress
        .send(PeerIngress::Command {
            request: request("read-only", timestamp(5)),
            claimed_principal: "admin".to_owned(),
            route_name: "admin/start".to_owned(),
        })
        .await
        .expect("peer command");
    assert!(matches!(
        timeout(peer.responses.recv()).await.expect("explicit response"),
        PeerResponse::Command(Err(error)) if error.code() == TargetPortErrorCode::Unsupported
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), fixture.host.next_command())
            .await
            .is_err()
    );

    peer.ingress
        .send(PeerIngress::Query(TargetQuery::StationSnapshot(station(
            "station-b",
        ))))
        .await
        .expect("query");
    assert!(matches!(
        timeout(peer.responses.recv()).await.expect("query response"),
        PeerResponse::Query(Err(error)) if error.code() == TargetPortErrorCode::Unauthorized
    ));
    assert_eq!(source.call_count(), 0);

    fixture.host.request_shutdown();
    timeout(task).await.expect("join").expect("target");
}

fn descriptor(read_only: bool) -> TargetDescriptor {
    TargetDescriptor {
        kind: text(
            TargetKind::new,
            if read_only {
                "test.read-only"
            } else {
                "test.capable"
            },
        ),
        instance_id: text(TargetInstanceId::new, "target-main"),
        contract_version: ContractVersion::V1_INITIAL,
        outbound_message_classes: vec![TargetMessageClass::CommandResult],
        inbound_operations: if read_only {
            vec![]
        } else {
            vec![Operation::Start]
        },
        limits: TargetLimits {
            maximum_message_bytes: 1024,
            maximum_in_flight_deliveries: 2,
            maximum_in_flight_commands: usize::from(!read_only),
        },
        delivery_semantics: vec![DeliverySemantic::NamedPeerAcknowledgement],
        optional_capabilities: vec![],
    }
}

fn delivery(number: u8) -> TargetDelivery<()> {
    let command = ExternalCommand::authenticated(
        request(&format!("delivery-{number}"), timestamp(5)),
        trusted_origin(),
    );
    TargetDelivery {
        delivery_id: text(
            uob_application::DeliveryId::new,
            format!("delivery-{number}"),
        ),
        target_instance_id: text(TargetInstanceId::new, "target-main"),
        target_configuration_revision: 1,
        station_ordering_key: station("station-a"),
        deadline: timestamp(5),
        class: TargetDeliveryClass::Durable,
        message: Arc::new(TargetMessage::CommandResult(result_for(&command))),
    }
}

fn request(id: &str, expires_at: UtcTimestamp) -> CommandRequest<()> {
    CommandRequest {
        request_id: text(RequestId::new, id),
        correlation_id: None,
        resource: station("station-a"),
        operation: CommandOperation::Start {
            authorization_reference: None,
        },
        expires_at,
    }
}

fn result_for(command: &ExternalCommand<()>) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.request.correlation_id.clone(),
        resource: command.request.resource.clone(),
        return_route: CommandReturnRoute {
            request_id: command.request.request_id.clone(),
            origin: command.origin.clone(),
        },
        lifecycle: CommandLifecycle::Admitted,
        recorded_at: timestamp(1),
        observed_effects: vec![],
    }
}

fn trusted_origin() -> AuthenticatedCommandOrigin {
    AuthenticatedCommandOrigin::Target {
        target_instance_id: text(TargetInstanceId::new, "target-main"),
        principal_id: text(PrincipalId::new, "configured-reader"),
    }
}

fn station(id: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-1"),
        station_id: text(StationId::new, id),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp(minute: i64) -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(minute))
}

async fn timeout<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(1), future)
        .await
        .expect("scenario timed out")
}

fn text<T, E: std::fmt::Debug>(
    constructor: impl FnOnce(String) -> Result<T, E>,
    value: impl Into<String>,
) -> T {
    constructor(value.into()).expect("valid fixture text")
}
