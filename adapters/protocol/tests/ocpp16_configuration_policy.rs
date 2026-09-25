#![allow(dead_code)]
mod endpoint_support;
#[path = "ocpp16_remote_control/support.rs"]
mod support;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use support::*;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::{
    LocalConfigurationValues, LocalRemoteStartIdentity, ProtectedConfigurationText,
    ProtectedConfigurationValue, RemoteControlSession,
};

const REFERENCE: &str = "cfg:1111111111111111111111111111111111111111111111111111111111111111";

fn policy_values(snapshot: &StationSnapshot) -> Arc<LocalConfigurationValues> {
    let mut other = snapshot.station.clone();
    other.station_id = StationId::new("other-station").unwrap();
    Arc::new(
        LocalConfigurationValues::new(vec![
            ProtectedConfigurationValue {
                resource: other,
                key: "VendorPassword".to_owned(),
                reference: REFERENCE.to_owned(),
                value: ProtectedConfigurationText::new("private-secret".to_owned()).unwrap(),
                expires_at: time("2026-09-02T00:00:00Z"),
            },
            ProtectedConfigurationValue {
                resource: snapshot.station.clone(),
                key: "VendorPassword".to_owned(),
                reference: format!("cfg:{}", "2".repeat(64)),
                value: ProtectedConfigurationText::new("private-secret".to_owned()).unwrap(),
                expires_at: time("2026-09-01T01:00:00Z"),
            },
        ])
        .unwrap(),
    )
}

#[tokio::test]
async fn protected_reference_does_not_cross_station_expiry_or_disconnected_socket() {
    let running = session("ocpp1.6", Duration::from_secs(2)).await;
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
    let provider = policy_values(&snapshot);
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
    let commands = scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    );
    for (id, reference) in [
        ("other-resource", REFERENCE.to_owned()),
        ("expired", format!("cfg:{}", "2".repeat(64))),
    ] {
        let mut command = command(
            &snapshot,
            id,
            protocol(
                "ChangeConfiguration",
                json!({"key":"VendorPassword","valueReference":reference}),
            ),
        );
        command.request.resource = snapshot.station.clone();
        if let CommandOperation::Ocpp(operation) = &mut command.request.operation {
            operation.payload_schema =
                PayloadSchemaId::new("urn:uob:ocpp16:ChangeConfigurationReference:1").unwrap();
        }
        let result = commands.submit(command).await.unwrap();
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::Rejected {
                error: CommandError {
                    code: CommandErrorCode::PolicyRejected,
                    ..
                }
            }
        ));
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    let mut offline = command(
        &snapshot,
        "offline",
        protocol("GetConfiguration", json!({})),
    );
    offline.request.resource = snapshot.station.clone();
    let result = commands.submit(offline).await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::StationDisconnected,
                ..
            }
        }
    ));
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
