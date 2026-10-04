use super::*;
use uob_protocol_adapter::{OutboundCall, SessionCallOutcome};

#[tokio::test]
async fn protected_reference_metadata_participates_in_the_shared_queue_budget() {
    let limits = RuntimeResourceLimits {
        // Leave room for the inbound decoder's 4KiB overhead and the small query reply,
        // but not the protected update's complete retained reference/resource metadata.
        aggregate_queued_payload_bytes: 6 * 1024,
        reserved_critical_payload_bytes: 1024,
        trace_ring_bytes: 0,
        ..RuntimeResourceLimits::default()
    };
    let mut running = session_with_limits("ocpp1.6", Duration::from_secs(2), Some(limits)).await;
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
    let port = RemoteControlSession::new(
        running.handle.clone(),
        snapshot.clone(),
        Arc::new(LocalRemoteStartIdentity::new(vec![], auth).unwrap()),
        Arc::new(Clock),
        Arc::new(store.clone()),
    )
    .unwrap()
    .with_local_authorization_updates(material::provider(&snapshot.station));
    assert!(matches!(
        port.dispatch(request(&snapshot, "budgeted-protected", "SendLocalList").admit(Clock.now()))
            .await
            .unwrap(),
        CommandDispatchOutcome::NotTransmitted {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
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
    assert!(matches!(
        sentinel.receive().await,
        SessionCallOutcome::Result { payload, .. } if payload == json!({"listVersion":0})
    ));
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn unpaired_and_delayed_native_replies_do_not_repair_an_uncertain_update_or_replay_it() {
    let mut running = session("ocpp1.6", Duration::from_millis(100)).await;
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
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "GetLocalListVersion".to_owned(),
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
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port,
        Arc::new(Clock),
    ));
    let update = request(&snapshot, "late-update", "SendLocalList");
    let pending = {
        let coordinator = coordinator.clone();
        let update = update.clone();
        tokio::spawn(async move { coordinator.submit(update).await.unwrap() })
    };
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2, "late-update", "SendLocalList", material::native()])
    );
    running
        .peer
        .send_text(json!([3,"foreign-message",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let uncertain = pending.await.unwrap();
    assert!(matches!(
        uncertain.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert!(uncertain.local_authorization_16.is_none());
    running
        .peer
        .send_text(json!([3,"late-update",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let query = {
        let coordinator = coordinator.clone();
        let query = request(&snapshot, "independent-query", "GetLocalListVersion");
        tokio::spawn(async move { coordinator.submit(query).await.unwrap() })
    };
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2, "independent-query", "GetLocalListVersion", {}])
    );
    running
        .peer
        .send_text(json!([3,"independent-query",{"listVersion":-2}]).to_string())
        .await
        .unwrap();
    assert!(query.await.unwrap().local_authorization_16.is_some());
    assert_eq!(coordinator.submit(update).await.unwrap(), uncertain);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
