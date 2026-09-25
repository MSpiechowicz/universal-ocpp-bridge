use super::*;
use futures::poll;
use std::sync::atomic::{AtomicBool, Ordering};
use uob_protocol_adapter::OutboundCall;

const EXPIRED_REFERENCE: &str =
    "cfg:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

struct AdvancingClock(AtomicBool);

impl CommandClock for AdvancingClock {
    fn now(&self) -> UtcTimestamp {
        if self.0.load(Ordering::SeqCst) {
            time("2026-09-01T04:00:00Z")
        } else {
            time("2026-09-01T02:00:00Z")
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn queued_configuration_rechecks_revocation_and_expiry_before_socket_send() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, authorization, _, _) = setup(&store, running.handle.clone()).await;
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "ChangeConfiguration".to_owned(),
        },
        parameters: vec![],
    });
    let identity = Arc::new(
        LocalRemoteStartIdentity::new(
            vec![SensitiveAuthorizationToken::new("INDEPENDENT-001").unwrap()],
            authorization,
        )
        .unwrap(),
    );
    let clock = Arc::new(AdvancingClock(AtomicBool::new(false)));
    let mut expiring = protected(&snapshot, "ConnectionTimeOut", "expiry-secret");
    expiring.reference = EXPIRED_REFERENCE.to_owned();
    expiring.expires_at = time("2026-09-01T03:00:00Z");
    let provider = Arc::new(
        LocalConfigurationValues::new(vec![
            protected(&snapshot, "VendorPassword", "revoked-secret"),
            expiring,
        ])
        .unwrap(),
    );
    let port = RemoteControlSession::new(
        running.handle.clone(),
        snapshot.clone(),
        identity,
        clock.clone(),
        Arc::new(store.clone()),
    )
    .unwrap()
    .with_configuration_values(provider.clone());

    // Poll only the producer on this single-thread runtime: both calls enter the bounded
    // socket queue, but the socket owner cannot run until the test next awaits.
    let revoked_command =
        write(&snapshot, "queued-revoked", "VendorPassword", REFERENCE).admit(clock.now());
    let expired_command = write(
        &snapshot,
        "queued-expired",
        "ConnectionTimeOut",
        EXPIRED_REFERENCE,
    )
    .admit(clock.now());
    let mut revoked = Box::pin(port.dispatch(revoked_command));
    let mut expired = Box::pin(port.dispatch(expired_command));
    assert!(poll!(revoked.as_mut()).is_pending());
    assert!(poll!(expired.as_mut()).is_pending());

    provider.revoke(REFERENCE).unwrap();
    clock.0.store(true, Ordering::SeqCst);
    for result in [revoked.await.unwrap(), expired.await.unwrap()] {
        assert!(matches!(
            result,
            CommandDispatchOutcome::NotTransmitted {
                error: CommandError {
                    code: CommandErrorCode::PolicyRejected,
                    ..
                }
            }
        ));
    }

    // A later ordinary call proves the peer did not receive either protected CALL.
    let sentinel = running
        .handle
        .try_call(OutboundCall {
            message_id: "queue-sentinel".to_owned(),
            action: ProtocolActionName::new("GetConfiguration").unwrap(),
            payload: json!({}),
            correlation_id: CorrelationId::new("queue-sentinel").unwrap(),
        })
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2, "queue-sentinel", "GetConfiguration", {}])
    );
    running
        .peer
        .send_text(json!([3, "queue-sentinel", {}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        sentinel.receive().await,
        uob_protocol_adapter::SessionCallOutcome::Result { .. }
    ));

    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
