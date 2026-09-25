#![allow(dead_code)]
mod endpoint_support;
#[path = "ocpp16_remote_control/support.rs"]
mod support;

use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::{
    LocalConfigurationValues, LocalRemoteStartIdentity, ProtectedConfigurationText,
    ProtectedConfigurationValue, RemoteControlSession,
};

const REFERENCE: &str = "cfg:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn write(snapshot: &StationSnapshot, id: &str, reference: &str) -> ExternalCommand<Value> {
    let mut external = command(
        snapshot,
        id,
        protocol(
            "ChangeConfiguration",
            json!({"key":"HeartbeatInterval","valueReference":reference}),
        ),
    );
    external.request.resource = snapshot.station.clone();
    if let CommandOperation::Ocpp(operation) = &mut external.request.operation {
        operation.payload_schema =
            PayloadSchemaId::new("urn:uob:ocpp16:ChangeConfigurationReference:1").unwrap();
    }
    external
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep native statuses and restart/deduplication evidence together.
async fn exact_write_statuses_deduplication_and_invalid_response_never_trigger_retry() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, auth, _, _) = setup(&store, running.handle.clone()).await;
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "ChangeConfiguration".to_owned(),
        },
        parameters: vec![],
    });
    let empty_reference = format!("cfg:{}", "3".repeat(64));
    let provider = Arc::new(
        LocalConfigurationValues::new(vec![
            ProtectedConfigurationValue {
                resource: snapshot.station.clone(),
                key: "HeartbeatInterval".to_owned(),
                reference: REFERENCE.to_owned(),
                value: ProtectedConfigurationText::new("90".to_owned()).unwrap(),
                expires_at: time("2026-09-02T00:00:00Z"),
            },
            ProtectedConfigurationValue {
                resource: snapshot.station.clone(),
                key: "ConnectionTimeOut".to_owned(),
                reference: empty_reference.clone(),
                value: ProtectedConfigurationText::new(String::new()).unwrap(),
                expires_at: time("2026-09-02T00:00:00Z"),
            },
        ])
        .unwrap(),
    );
    let identity = Arc::new(
        LocalRemoteStartIdentity::new(
            vec![SensitiveAuthorizationToken::new("INDEPENDENT-001").unwrap()],
            auth,
        )
        .unwrap(),
    );
    let port = Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            identity,
            Arc::new(Clock),
            Arc::new(store.clone()),
        )
        .unwrap()
        .with_configuration_values(provider),
    );
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port,
        Arc::new(Clock),
    ));
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    for (id, status, accepted) in [
        ("write-accepted", "Accepted", true),
        ("write-rejected", "Rejected", false),
        ("write-reboot", "RebootRequired", true),
        ("write-unsupported", "NotSupported", false),
    ] {
        let requested = write(&snapshot, id, REFERENCE);
        let sender = commands.clone();
        let submission = requested.clone();
        let pending = tokio::spawn(async move { sender.submit(submission).await.unwrap() });
        assert_eq!(
            receive_json(&mut running.peer).await,
            json!([2,id,"ChangeConfiguration",{"key":"HeartbeatInterval","value":"90"}])
        );
        running
            .peer
            .send_text(json!([3,id,{"status":status}]).to_string())
            .await
            .unwrap();
        let result = pending.await.unwrap();
        assert!(
            matches!(result.lifecycle, CommandLifecycle::ProtocolResponse { accepted: outcome, .. } if outcome == accepted)
        );
        assert_eq!(
            serde_json::to_value(&result.configuration).unwrap()["status"],
            json!(status)
        );
        let retry = commands.submit(requested).await.unwrap();
        assert_eq!(retry, result);
        assert!(
            timeout(Duration::from_millis(80), running.peer.receive())
                .await
                .is_err(),
            "duplicate must not be retransmitted"
        );
    }
    let mut empty = write(&snapshot, "write-empty", &empty_reference);
    if let CommandOperation::Ocpp(operation) = &mut empty.request.operation {
        operation.payload["key"] = json!("ConnectionTimeOut");
    }
    let sender = commands.clone();
    let pending = tokio::spawn(async move { sender.submit(empty).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2,"write-empty","ChangeConfiguration",{"key":"ConnectionTimeOut","value":""}])
    );
    running
        .peer
        .send_text(json!([3,"write-empty",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    accepted(&pending.await.unwrap());
    let invalid = write(&snapshot, "write-uncertain", REFERENCE);
    let sender = commands.clone();
    let request = invalid.clone();
    let pending = tokio::spawn(async move { sender.submit(request).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await[2],
        json!("ChangeConfiguration")
    );
    running
        .peer
        .send_text(json!([3,"write-uncertain",{"status":"Unexpected"}]).to_string())
        .await
        .unwrap();
    let uncertain = pending.await.unwrap();
    assert!(matches!(
        uncertain.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert!(uncertain.configuration.is_none());
    assert_eq!(commands.submit(invalid.clone()).await.unwrap(), uncertain);
    assert!(
        timeout(Duration::from_millis(80), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let restored = database.open();
    assert_eq!(
        restored
            .command_result_by_request_id(RequestId::new("write-uncertain").unwrap())
            .await
            .unwrap(),
        Some(uncertain.clone())
    );
    let mut next = session("ocpp1.6", Duration::from_secs(1)).await;
    let (mut next_snapshot, next_auth, _, _) = setup(&restored, next.handle.clone()).await;
    next_snapshot
        .capabilities
        .operations
        .push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: "ChangeConfiguration".to_owned(),
            },
            parameters: vec![],
        });
    let next_identity = Arc::new(
        LocalRemoteStartIdentity::new(
            vec![SensitiveAuthorizationToken::new("INDEPENDENT-001").unwrap()],
            next_auth,
        )
        .unwrap(),
    );
    let next_port = Arc::new(
        RemoteControlSession::new(
            next.handle.clone(),
            next_snapshot.clone(),
            next_identity,
            Arc::new(Clock),
            Arc::new(restored.clone()),
        )
        .unwrap(),
    );
    let next_coordinator = Arc::new(Coordinator::new(
        Arc::new(restored.clone()),
        next_port,
        Arc::new(Clock),
    ));
    let next_commands = scoped(
        next_coordinator.clone(),
        &next_snapshot,
        vec![AccessPermission::PrivilegedControl],
    );
    let recovered = next_coordinator
        .recover_unresolved(PageLimit::new(100).unwrap())
        .await
        .unwrap();
    assert!(
        recovered
            .commands
            .iter()
            .any(|row| row.command.request_id.as_str() == "write-uncertain"
                && row.result == uncertain)
    );
    assert_eq!(next_commands.submit(invalid).await.unwrap(), uncertain);
    assert!(
        timeout(Duration::from_millis(80), next.peer.receive())
            .await
            .is_err(),
        "restart must not replay uncertain write"
    );
    next.task.shutdown(Duration::from_secs(1)).await.unwrap();
    next.server.abort();
    restored.shutdown(Duration::from_secs(1)).await.unwrap();
}
