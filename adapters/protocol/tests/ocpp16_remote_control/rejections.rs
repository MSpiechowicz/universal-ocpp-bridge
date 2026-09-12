use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::{
    OutboundCall, SessionCallOutcome, v16::remote_control::RemoteControlSession,
};

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the ordered wire/durable assertions together.
async fn invalid_expired_unavailable_and_unprivileged_commands_never_reach_wire() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, auth, port, coordinator) = setup(&store, running.handle.clone()).await;
    let controls = scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::Control],
    );
    let reset = command(
        &snapshot,
        "denied-reset",
        protocol("Reset", json!({"type":"Hard"})),
    );
    assert!(controls.submit(reset.clone()).await.is_err());
    assert!(
        store
            .command_by_request_id(reset.request.request_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    let all = scoped(
        coordinator.clone(),
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    );
    let mut cases = vec![
        (
            command(
                &snapshot,
                "bad-reset",
                protocol("Reset", json!({"type":"Immediate"})),
            ),
            CommandErrorCode::InvalidParameters,
        ),
        (
            command(
                &snapshot,
                "extra",
                protocol("Reset", json!({"type":"Hard","extra":"ignored?"})),
            ),
            CommandErrorCode::InvalidParameters,
        ),
        (
            command(
                &snapshot,
                "wrong-connector",
                protocol("UnlockConnector", json!({"connectorId":2})),
            ),
            CommandErrorCode::InvalidParameters,
        ),
        (
            command(
                &snapshot,
                "zero",
                protocol("UnlockConnector", json!({"connectorId":0})),
            ),
            CommandErrorCode::InvalidParameters,
        ),
        (
            command(
                &snapshot,
                "unknown-transaction",
                CommandOperation::Stop {
                    transaction_id: TransactionId::new("1").unwrap(),
                },
            ),
            CommandErrorCode::InvalidParameters,
        ),
        (
            command(
                &snapshot,
                "missing-token",
                CommandOperation::Start {
                    authorization_reference: None,
                },
            ),
            CommandErrorCode::PolicyRejected,
        ),
        (
            command(
                &snapshot,
                "unknown-token",
                CommandOperation::Start {
                    authorization_reference: Some("unknown".to_owned()),
                },
            ),
            CommandErrorCode::PolicyRejected,
        ),
    ];
    let mut expired = reset.clone();
    expired.request.request_id = RequestId::new("expired").unwrap();
    expired.request.expires_at = Clock.now();
    cases.push((expired, CommandErrorCode::Expired));
    let mut schema = reset.clone();
    schema.request.request_id = RequestId::new("schema").unwrap();
    if let CommandOperation::Ocpp(op) = &mut schema.request.operation {
        op.payload_schema = PayloadSchemaId::new("wrong").unwrap();
    }
    cases.push((schema, CommandErrorCode::UnsupportedOperation));
    let mut spoofed = reset.clone();
    spoofed.request.request_id = RequestId::new("spoofed").unwrap();
    spoofed.request.resource.native_protocol_reference =
        Some(NativeProtocolReference::Ocpp16 { connector_id: 1 });
    cases.push((spoofed, CommandErrorCode::StationDisconnected));
    let mut unsupported = reset;
    unsupported.request.request_id = RequestId::new("unsupported").unwrap();
    unsupported.request.resource = snapshot.resources[1].resource.clone();
    cases.push((unsupported, CommandErrorCode::UnsupportedOperation));
    for (external, expected) in cases {
        let result = all.submit(external).await.unwrap();
        assert!(
            matches!(result.lifecycle,CommandLifecycle::Rejected {error} if error.code==expected),
            "expected {expected:?}"
        );
    }
    let start = command(
        &snapshot,
        "unavailable",
        CommandOperation::Start {
            authorization_reference: Some(reference().await.as_str().to_owned()),
        },
    );
    for state in [
        AvailabilityState::Unavailable,
        AvailabilityState::Faulted,
        AvailabilityState::Unknown,
        AvailabilityState::Occupied,
    ] {
        let mut unavailable = snapshot.clone();
        unavailable.resources[0].availability = state;
        port.update_committed(unavailable).unwrap();
        let mut request = start.clone();
        request.request.request_id = RequestId::new(format!("state-{state:?}")).unwrap();
        assert!(
            matches!(all.submit(request).await.unwrap().lifecycle,CommandLifecycle::Rejected {error} if error.code==CommandErrorCode::PolicyRejected)
        );
    }
    port.update_committed(snapshot.clone()).unwrap();
    auth.apply_change(AuthorizationChange {
        reference: reference().await,
        resource: snapshot.resources[0].resource.clone(),
        state: AuthorizationState::Revoked,
        revision: 2,
        changed_at: Clock.now(),
        expires_at: None,
    })
    .await
    .unwrap();
    assert!(
        matches!(all.submit(start).await.unwrap().lifecycle,CommandLifecycle::Rejected {error} if error.code==CommandErrorCode::PolicyRejected)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn expired_socket_queue_and_disconnected_sessions_do_not_replay() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup(&store, running.handle.clone()).await;
    let pending = running
        .handle
        .try_call_before(
            OutboundCall {
                message_id: "past".to_owned(),
                action: ProtocolActionName::new("Reset").unwrap(),
                payload: json!({"type":"Soft"}),
                correlation_id: CorrelationId::new("past").unwrap(),
            },
            tokio::time::Instant::now(),
        )
        .unwrap();
    assert!(matches!(
        pending.receive().await,
        SessionCallOutcome::NotTransmitted { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    let external = command(
        &snapshot,
        "offline",
        protocol("Reset", json!({"type":"Soft"})),
    );
    assert!(
        matches!(coordinator.submit(external.clone()).await.unwrap().lifecycle,CommandLifecycle::Rejected {error} if error.code==CommandErrorCode::StationDisconnected)
    );
    assert!(
        store
            .command_by_request_id(external.request.request_id)
            .await
            .unwrap()
            .is_none()
    );
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn snapshot_identity_epoch_and_protocol_are_bound_to_the_actual_socket() {
    let running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, auth, port, _) = setup(&store, running.handle.clone()).await;
    let mut other = snapshot.clone();
    other.station.station_id = StationId::new("other").unwrap();
    assert!(port.update_committed(other).is_err());
    let mut epoch = snapshot.clone();
    if let Connectivity::Connected { connected_at, .. } = &mut epoch.connectivity {
        *connected_at = Clock.now();
    }
    assert!(port.update_committed(epoch).is_err());
    let mut old = snapshot.clone();
    old.observed_at = time("2020-01-01T00:00:00Z");
    assert!(port.update_committed(old).is_err());
    let other = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let identity = Arc::new(
        uob_protocol_adapter::v16::remote_control::LocalRemoteStartIdentity::new(vec![], auth)
            .unwrap(),
    );
    assert!(RemoteControlSession::new(other.handle, snapshot, identity, Arc::new(Clock)).is_err());
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    other.task.shutdown(Duration::from_secs(1)).await.unwrap();
    other.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
