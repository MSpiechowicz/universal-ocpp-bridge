#![allow(dead_code)]
mod endpoint_support;
#[path = "ocpp16_local_authorization/frame_limit.rs"]
mod frame_limit;
#[path = "ocpp16_local_authorization/material.rs"]
mod material;
#[path = "ocpp16_local_authorization/queue.rs"]
mod queue;
#[path = "ocpp16_remote_control/support.rs"]
mod support;
#[path = "ocpp16_local_authorization/timing.rs"]
mod timing;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::*;

fn request(snapshot: &StationSnapshot, id: &str, action: &str) -> ExternalCommand<Value> {
    let payload = if action == "SendLocalList" {
        json!({"listVersion":-2,"updateType":"Full","updateReference":material::reference()})
    } else {
        json!({})
    };
    let mut external = command(snapshot, id, protocol(action, payload));
    external.request.resource = snapshot.station.clone();
    if action == "SendLocalList"
        && let CommandOperation::Ocpp(operation) = &mut external.request.operation
    {
        operation.payload_schema =
            PayloadSchemaId::new(SEND_LOCAL_LIST_REFERENCE_SCHEMA_16).unwrap();
    }
    external
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Observe native replies, deduplication and durable recovery together.
async fn native_statuses_versions_and_uncertainty_are_durable_without_secret_evidence() {
    let mut running = session("ocpp1.6", Duration::from_millis(200)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, auth, _, _) = setup(&store, running.handle.clone()).await;
    for action in ["GetLocalListVersion", "SendLocalList", "ClearCache"] {
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: action.to_owned(),
            },
            parameters: vec![],
        });
    }
    let provider = material::provider(&snapshot.station);
    let identity = Arc::new(LocalRemoteStartIdentity::new(vec![], auth).unwrap());
    let port = Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            identity,
            Arc::new(Clock),
            Arc::new(store.clone()),
        )
        .unwrap()
        .with_local_authorization_updates(provider),
    );
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port,
        Arc::new(Clock),
    ));
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    let cases = [
        (
            "full-accepted",
            "SendLocalList",
            json!({"status":"Accepted"}),
        ),
        ("full-failed", "SendLocalList", json!({"status":"Failed"})),
        (
            "full-unsupported",
            "SendLocalList",
            json!({"status":"NotSupported"}),
        ),
        (
            "full-mismatch",
            "SendLocalList",
            json!({"status":"VersionMismatch"}),
        ),
        (
            "query-empty",
            "GetLocalListVersion",
            json!({"listVersion":0}),
        ),
        (
            "query-unsupported",
            "GetLocalListVersion",
            json!({"listVersion":-1}),
        ),
        (
            "query-negative",
            "GetLocalListVersion",
            json!({"listVersion":-2}),
        ),
        ("cache-accepted", "ClearCache", json!({"status":"Accepted"})),
        ("cache-rejected", "ClearCache", json!({"status":"Rejected"})),
        (
            "query-extra",
            "GetLocalListVersion",
            json!({"listVersion":7,"idTag":"PRIVATE-MARKER"}),
        ),
        (
            "query-overflow",
            "GetLocalListVersion",
            json!({"listVersion":2_147_483_648_u64}),
        ),
        ("cache-malformed", "ClearCache", json!({"status":"Failed"})),
    ];
    let mut retained = Vec::new();
    for (id, action, reply) in cases {
        let requested = request(&snapshot, id, action);
        let sender = commands.clone();
        let submission = requested.clone();
        let pending = tokio::spawn(async move { sender.submit(submission).await.unwrap() });
        let wire = receive_json(&mut running.peer).await;
        assert_eq!(wire[1], id);
        assert_eq!(wire[2], action);
        if action == "SendLocalList" {
            assert_eq!(wire[3], material::native());
        } else {
            assert_eq!(wire[3], json!({}));
        }
        running
            .peer
            .send_text(json!([3, id, reply]).to_string())
            .await
            .unwrap();
        let result = pending.await.unwrap();
        if id.ends_with("extra") || id.ends_with("overflow") || id.ends_with("malformed") {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::TransmissionUncertain { .. }
            ));
            assert!(result.local_authorization_16.is_none());
        } else {
            assert_eq!(
                result.schema_version,
                ContractVersion::V1_LOCAL_AUTHORIZATION_16
            );
            assert!(result.local_authorization_16.is_some());
        }
        let public = serde_json::to_string(&result).unwrap();
        for secret in ["PRIVATE-MARKER", "PARENT-MARKER", "list16:"] {
            assert!(!public.contains(secret));
        }
        assert_eq!(commands.submit(requested).await.unwrap(), result);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), running.peer.receive())
                .await
                .is_err()
        );
        retained.push(result);
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    for expected in retained {
        assert_eq!(
            reopened
                .command_result_by_request_id(expected.return_route.request_id.clone())
                .await
                .unwrap(),
            Some(expected)
        );
    }
    for suffix in ["state.db", "state.db-wal"] {
        if let Ok(bytes) = std::fs::read(database.0.join(suffix)) {
            for marker in ["PRIVATE-MARKER", "PARENT-MARKER"] {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                );
            }
        }
    }
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
