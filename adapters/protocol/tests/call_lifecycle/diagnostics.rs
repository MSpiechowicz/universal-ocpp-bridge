use super::*;
use std::sync::Arc;
use uob_application::{capture::*, *};
use uob_contracts::*;
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        serde_json::from_value(json!("2026-09-11T00:00:00Z")).unwrap()
    }
}
struct Station {
    handle: CallSessionHandle,
    protocol: ProtocolEdition,
}
impl StationCommandPort<Value> for Station {
    fn context(&self, _: ResourceRef) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async move {
            Ok(Some(StationCommandContext {
                connectivity: Connectivity::Connected {
                    protocol: self.protocol,
                    connected_at: Clock.now(),
                    last_message_at: None,
                },
                capabilities: ResourceCapabilities {
                    operations: vec![SupportedOperation {
                        operation: Operation::ProtocolAction {
                            protocol: self.protocol,
                            action: "Reset".into(),
                        },
                        parameters: vec![],
                    }],
                    ..ResourceCapabilities::default()
                },
            }))
        })
    }
    fn dispatch(
        &self,
        command: Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            let CommandOperation::Ocpp(operation) = command.operation else {
                panic!("test reset")
            };
            let pending = self
                .handle
                .try_call(OutboundCall {
                    message_id: command.request_id.as_str().into(),
                    action: operation.action,
                    payload: operation.payload,
                    correlation_id: command.correlation_id.unwrap(),
                })
                .unwrap();
            Ok(match pending.receive().await {
                SessionCallOutcome::Result { payload, .. } => {
                    CommandDispatchOutcome::ProtocolResponse {
                        accepted: payload["status"] == "Accepted",
                        error: None,
                    }
                }
                _ => CommandDispatchOutcome::TransmissionUncertain {
                    detail: "test response missing".into(),
                },
            })
        })
    }
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the complete wire scenario and evidence assertions together.
async fn target_command_trace_crosses_durable_and_socket_awaits_for_both_editions() {
    for (wire, protocol) in [
        ("ocpp1.6", ProtocolEdition::Ocpp16j),
        ("ocpp2.0.1", ProtocolEdition::Ocpp201),
    ] {
        let app = endpoint_support::application(Environment::Demo, None);
        let capture = CaptureManager::new(true);
        let grant = CaptureGrant::new(
            app.identity().bridge_id.clone(),
            vec![CapturePermission::Capture],
            None,
            None,
        )
        .unwrap();
        capture
            .start(
                &grant,
                CaptureFilter {
                    bridge: app.identity().bridge_id.clone(),
                    station: None,
                    target: None,
                },
                CaptureLevel::Metadata,
                None,
            )
            .unwrap();
        let (flow, rx) = FlowDiagnostics::channel(
            app.runtime_identity().process_instance_id.clone(),
            app.identity().bridge_id.clone(),
            capture,
            Arc::new(Clock),
            128,
        )
        .unwrap();
        let mut running =
            session_with_diagnostics(wire, Duration::from_millis(100), flow.clone()).await;
        let root = std::env::temp_dir().join(format!("uob-flow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let store = Arc::new(
            uob_storage_adapter::SqliteOperationalStore::<Value, String, String, String>::open(
                root.join("state.db"),
                8,
            )
            .unwrap(),
        );
        let coordinator = Arc::new(
            CommandCoordinator::new(
                store.clone(),
                Arc::new(Station {
                    handle: running.handle.clone(),
                    protocol,
                }),
                Arc::new(Clock),
            )
            .with_diagnostics(flow.clone()),
        );
        let origin = AuthenticatedCommandOrigin::Target {
            target_instance_id: TargetInstanceId::new("target").unwrap(),
            principal_id: PrincipalId::new("peer").unwrap(),
        };
        let resource = ResourceRef {
            bridge_id: app.identity().bridge_id.clone(),
            station_id: StationId::new("alpha").unwrap(),
            resource: None,
            native_protocol_reference: None,
        };
        let policy = AccessPolicy::single(
            AccessGrant::new(
                origin.clone(),
                vec![AccessPermission::PrivilegedControl],
                vec![AccessResourceScope::Resource(resource.clone())],
            )
            .unwrap(),
        );
        let admission =
            Arc::new(ScopedCommandAdmissionPort::new(coordinator, policy).with_diagnostics(flow));
        for (id, reply) in [("accepted", true), ("missing", false)] {
            let command = ExternalCommand::authenticated(
                CommandRequest {
                    request_id: RequestId::new(id).unwrap(),
                    correlation_id: Some(CorrelationId::new(id).unwrap()),
                    resource: resource.clone(),
                    operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
                        protocol,
                        action: ProtocolActionName::new("Reset").unwrap(),
                        payload_schema: PayloadSchemaId::new("test-reset-v1").unwrap(),
                        payload: json!({"type":"Immediate"}),
                    }),
                    expires_at: serde_json::from_value(json!("2026-09-12T00:00:00Z")).unwrap(),
                },
                origin.clone(),
            );
            let port = admission.clone();
            let result = tokio::spawn(async move { port.submit(command).await.unwrap() });
            assert_eq!(receive_json(&mut running.peer).await[2], "Reset");
            if reply {
                running
                    .peer
                    .send_text(json!([3,id,{"status":"Accepted"}]).to_string())
                    .await
                    .unwrap();
            }
            let result = result.await.unwrap();
            assert_eq!(
                matches!(
                    result.lifecycle,
                    CommandLifecycle::ProtocolResponse { accepted: true, .. }
                ),
                reply
            );
            let traces = rx
                .try_iter()
                .map(|r| serde_json::from_slice::<TraceRecord>(r.encoded_json()).unwrap())
                .collect::<Vec<_>>();
            assert!(
                traces
                    .iter()
                    .all(|r| r.correlation_id.as_ref().unwrap().as_str() == id)
            );
            for stage in [
                "command.authorization",
                "command.ingress",
                "command.dispatch",
                "command.protocol_response",
            ] {
                assert!(traces.iter().any(|trace| {
                    trace.stage.as_str() == stage
                        && trace
                            .redacted_details
                            .as_ref()
                            .unwrap()
                            .fields
                            .get("command.request_id")
                            .is_some_and(|request| request == id)
                }));
            }
            assert!(traces.iter().any(|trace| {
                trace
                    .redacted_details
                    .as_ref()
                    .unwrap()
                    .fields
                    .contains_key("command.origin")
            }));
            for stage in [
                "command.authorization",
                "command.ingress",
                "storage.commit",
                "command.dispatch",
                "ocpp.send",
                "command.protocol_response",
            ] {
                assert!(
                    traces.iter().any(|r| r.stage.as_str() == stage),
                    "missing {stage}"
                );
            }
            assert!(
                !traces
                    .iter()
                    .any(|r| r.stage.as_str() == "command.observed_effect")
            );
            assert_eq!(
                traces.iter().any(|r| r
                    .redacted_details
                    .as_ref()
                    .unwrap()
                    .fields
                    .get("evidence")
                    .is_some_and(|v| v == "charger_accepted")),
                reply
            );
        }
        running.task.shutdown(TEST_BOUND).await.unwrap();
        running.server.abort();
        drop(admission);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
