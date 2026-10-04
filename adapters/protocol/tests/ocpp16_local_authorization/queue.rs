use super::*;
use futures::poll;
use std::sync::atomic::{AtomicBool, Ordering};
use uob_protocol_adapter::{OutboundCall, SessionSubmitError};
struct AdvancingClock(AtomicBool);
impl CommandClock for AdvancingClock {
    fn now(&self) -> UtcTimestamp {
        if self.0.load(Ordering::SeqCst) {
            time("2026-09-02T00:00:00Z")
        } else {
            time("2026-09-01T02:00:00Z")
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn queued_update_rechecks_provider_expiry_revocation_and_socket_generation() {
    for mutation in 0..4 {
        let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
        let database = Database::new();
        let store = database.open();
        let (mut snapshot, auth, _, _) = setup(&store, running.handle.clone()).await;
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: "SendLocalList".to_owned(),
            },
            parameters: vec![],
        });
        let provider = material::provider(&snapshot.station);
        let clock = Arc::new(AdvancingClock(AtomicBool::new(false)));
        let port = RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            Arc::new(LocalRemoteStartIdentity::new(vec![], auth).unwrap()),
            clock.clone(),
            Arc::new(store.clone()),
        )
        .unwrap()
        .with_local_authorization_updates(provider.clone());
        let mut command =
            request(&snapshot, "queued-protected", "SendLocalList").admit(clock.now());
        command.expires_at = time("2026-09-03T00:00:00Z");
        let mut pending = Box::pin(port.dispatch(command));
        assert!(poll!(pending.as_mut()).is_pending());
        match mutation {
            0 => provider.revoke(&material::reference()).unwrap(),
            1 => clock.0.store(true, Ordering::SeqCst),
            2 => port.detach_local_authorization(),
            _ => {
                snapshot.capabilities.operations.retain(|entry| !matches!(&entry.operation, Operation::ProtocolAction { action, .. } if action == "SendLocalList"));
                port.update_committed(snapshot).unwrap();
            }
        }
        assert!(matches!(
            pending.await.unwrap(),
            CommandDispatchOutcome::NotTransmitted { .. }
        ));
        let sentinel = running
            .handle
            .try_call(OutboundCall {
                message_id: "sentinel".to_owned(),
                action: ProtocolActionName::new("GetLocalListVersion").unwrap(),
                payload: json!({}),
                correlation_id: CorrelationId::new("sentinel").unwrap(),
            })
            .unwrap();
        assert_eq!(
            receive_json(&mut running.peer).await,
            json!([2, "sentinel", "GetLocalListVersion", {}])
        );
        running
            .peer
            .send_text(json!([3,"sentinel",{"listVersion":0}]).to_string())
            .await
            .unwrap();
        sentinel.receive().await;
        running.task.shutdown(Duration::from_secs(1)).await.unwrap();
        running.server.abort();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}
#[tokio::test]
async fn actual_native_frame_not_reference_envelope_controls_queue_byte_admission() {
    let id = "sized\"\\";
    let exact_bytes = serde_json::to_string(&json!([2, id, "SendLocalList", material::native()]))
        .unwrap()
        .len();
    for maximum in [exact_bytes - 1, exact_bytes] {
        let limits = RuntimeResourceLimits {
            maximum_ocpp_message_bytes: maximum,
            ..RuntimeResourceLimits::default()
        };
        let mut running =
            session_with_limits("ocpp1.6", Duration::from_secs(2), Some(limits)).await;
        let database = Database::new();
        let store = database.open();
        let (mut snapshot, auth, _, _) = setup(&store, running.handle.clone()).await;
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: "SendLocalList".to_owned(),
            },
            parameters: vec![],
        });
        let port = Arc::new(
            RemoteControlSession::new(
                running.handle.clone(),
                snapshot.clone(),
                Arc::new(LocalRemoteStartIdentity::new(vec![], auth).unwrap()),
                Arc::new(Clock),
                Arc::new(store.clone()),
            )
            .unwrap()
            .with_local_authorization_updates(material::provider(&snapshot.station)),
        );
        assert!(matches!(
            running.handle.try_call(OutboundCall {
                message_id: "raw-bypass".to_owned(),
                action: ProtocolActionName::new("SendLocalList").unwrap(),
                payload: material::native(),
                correlation_id: CorrelationId::new("raw-bypass").unwrap()
            }),
            Err(SessionSubmitError::InvalidRequest)
        ));
        let command = request(&snapshot, id, "SendLocalList").admit(Clock.now());
        if maximum < exact_bytes {
            assert!(matches!(
                port.dispatch(command).await.unwrap(),
                CommandDispatchOutcome::NotTransmitted { .. }
            ));
        } else {
            let pending = tokio::spawn(async move { port.dispatch(command).await.unwrap() });
            assert_eq!(
                receive_json(&mut running.peer).await,
                json!([2, id, "SendLocalList", material::native()])
            );
            running
                .peer
                .send_text(json!([3,id,{"status":"Accepted"}]).to_string())
                .await
                .unwrap();
            assert!(matches!(
                pending.await.unwrap(),
                CommandDispatchOutcome::LocalAuthorizationResponse16(_)
            ));
        }
        running.task.shutdown(Duration::from_secs(1)).await.unwrap();
        running.server.abort();
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}
