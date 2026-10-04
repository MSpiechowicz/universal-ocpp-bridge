use std::time::Duration;

use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use uob_sim::local_authorization::LocalAuthorizationHandle;
use uob_sim::scenario::parse_configuration;
use uob_sim::{
    ProtocolClient, SimulatorAction, SimulatorCall, SimulatorClientError, SimulatorProtocolClient,
};

#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(5);

#[tokio::test]
async fn actual_native_handlers_never_accept_ephemeral_mutations() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (done, observed) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        send(
            &mut socket,
            json!([2, "unsupported-get", "GetLocalListVersion", {}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"unsupported-get",{"listVersion":-1}])
        );
        send(
            &mut socket,
            json!([2,"unsupported-send","SendLocalList",{
            "listVersion":1,"updateType":"Full","localAuthorizationList":[]}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"unsupported-send",{"status":"NotSupported"}])
        );
        send(
            &mut socket,
            json!([2, "unsupported-clear", "ClearCache", {}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"unsupported-clear",{"status":"Rejected"}])
        );
        done.send(()).unwrap();
        closed(&mut socket).await;
    });
    let source = format!(
        "schema_version=1\n[[stations]]\nid='unsupported-station'\nendpoint='ws://{address}'\nocpp_version='1.6'\n"
    );
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
        })
        .await
        .unwrap();
    timeout(BOUND, observed).await.unwrap().unwrap();
    assert_eq!(
        client.local_authorization().unwrap().snapshot()["listVersion"],
        -1
    );
    client.shutdown().await.unwrap();
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn synthetic_native_models_cannot_claim_durable_protocol_support() {
    let source = "schema_version=1\n[[stations]]\nid='memory-station'\nendpoint='ws://127.0.0.1:1'\nocpp_version='1.6'\n";
    let mut config = parse_configuration(source).unwrap().stations[0].client_config();
    config.local_authorization = Some(LocalAuthorizationHandle::in_memory("memory-station"));
    let result = SimulatorProtocolClient::connect(config).await;
    assert!(
        matches!(result, Err(SimulatorClientError::Protocol(message)) if message == "persistent native state required")
    );
}
