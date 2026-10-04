use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::timeout;

use super::{NativeReplayClient, finish_bootstrap};
use crate::local_authorization::{LocalAuthorizationConfig, LocalAuthorizationHandle};
use crate::{Ocpp16State, ProtocolClient, SimulatorAction, SimulatorCall, TraceBuffer};

#[path = "../tests/local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

const BOUND: Duration = Duration::from_secs(10);
const NATIVE_ID: i64 = 2_147_483_758;

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-boot-generation-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn model(&self) -> LocalAuthorizationHandle {
        let model = LocalAuthorizationHandle::open(
            "alpha",
            &LocalAuthorizationConfig {
                private_state_file: self.0.join("state.json").to_str().unwrap().to_owned(),
                list_supported: true,
                cache_supported: true,
            },
        )
        .unwrap();
        let update = serde_json::from_value(json!({"listVersion":7,"updateType":"Full",
            "localAuthorizationList":[{"idTag":"generation-private","idTagInfo":{"status":"Accepted"}}]})).unwrap();
        assert_eq!(
            model.update(&update),
            ocpp_client::ocpp_types::v16::common::SendLocalListResponseStatus::Accepted
        );
        assert!(
            model
                .offline_start(json!({"connectorId":1,"idTag":"GENERATION-PRIVATE",
            "meterStart":600,"timestamp":"2026-10-04T00:00:01Z"}))
                .unwrap()
        );
        model
            .offline_stop(
                1,
                json!({"meterStop":700,"timestamp":"2026-10-04T00:00:02Z"}),
            )
            .unwrap();
        model
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy)]
enum CompletionPath {
    Command,
    Bootstrap,
    FirstSend,
}
fn boot_call(model: &str) -> SimulatorCall {
    SimulatorCall {
        action: SimulatorAction::BootNotification,
        payload: json!({"chargePointVendor":"UOB","chargePointModel":model}),
    }
}

#[tokio::test]
async fn command_old_accepted_boot_cannot_register_or_replay_replacement_pending() {
    race(CompletionPath::Command, Some("Pending")).await;
}
#[tokio::test]
async fn command_old_accepted_boot_cannot_register_or_replay_replacement_rejected() {
    race(CompletionPath::Command, Some("Rejected")).await;
}
#[tokio::test]
async fn bootstrap_old_accepted_boot_cannot_register_or_replay_replacement_pending() {
    race(CompletionPath::Bootstrap, Some("Pending")).await;
}
#[tokio::test]
async fn bootstrap_old_accepted_boot_cannot_register_or_replay_replacement_rejected() {
    race(CompletionPath::Bootstrap, Some("Rejected")).await;
}
#[tokio::test]
async fn command_current_accepted_boot_still_replays_original_native_facts() {
    race(CompletionPath::Command, None).await;
}
#[tokio::test]
async fn bootstrap_current_accepted_boot_still_replays_original_native_facts() {
    race(CompletionPath::Bootstrap, None).await;
}

#[tokio::test]
async fn native_replay_start_queued_on_old_generation_never_crosses_to_rejected_socket() {
    race(CompletionPath::FirstSend, Some("Rejected")).await;
}

async fn race(path: CompletionPath, replacement: Option<&'static str>) {
    let directory = PrivateDirectory::new();
    let model = directory.model();
    let state = Arc::new(Mutex::new(Ocpp16State {
        local: Some(model.clone()),
        ..Ocpp16State::default()
    }));
    let traces = TraceBuffer::new(64);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (close_first, close_allowed) = oneshot::channel();
    let peer = tokio::spawn(native_peer(listener, replacement, close_allowed));
    let (client, second_socket) = setup_client(&endpoint, &state, &traces).await;
    let old_call = boot_call("Held old Boot");
    // SDK reception is real and complete, but the outer application completion
    // is deliberately held before the production post-await commit boundary.
    let exchange = crate::client_exchange16::NativeExchange::capture(&state, &old_call);
    let old_response = exchange.send(&client, &old_call).await.unwrap();
    assert_eq!(old_response["status"], "Accepted");
    let original_generation = state.lock().unwrap().socket_generation;
    let adapter = NativeReplayClient::new(client.clone(), Arc::clone(&state), traces.clone());
    let replacement_call = boot_call("Replacement Boot");
    if let Some(status) = replacement {
        close_first.send(()).unwrap();
        timeout(BOUND, second_socket).await.unwrap().unwrap();
        let current_adapter =
            NativeReplayClient::new(client.clone(), Arc::clone(&state), traces.clone());
        let response = current_adapter
            .call(replacement_call.clone())
            .await
            .unwrap();
        assert_eq!(response["status"], status);
        let current = state.lock().unwrap();
        assert!(current.socket_generation > original_generation);
        assert!(!current.registered);
        assert_eq!(current.boot.as_ref(), Some(&replacement_call.payload));
        assert!(!current.replay_requested);
    } else {
        drop(close_first);
        drop(second_socket);
    }
    let (release, held) = oneshot::channel();
    let completion = hold_completion(path, adapter, old_call, old_response, held);
    release.send(()).unwrap();
    let result = timeout(BOUND, completion).await.unwrap().unwrap();
    let heartbeat = client
        .send_heartbeat(ocpp_client::ocpp_types::v16::HeartbeatRequest {})
        .await
        .unwrap();
    assert_eq!(heartbeat.current_time.to_string(), "2026-10-04T00:00:03Z");
    let (registered, remembered_boot, accepted_generation, replay_requested) = {
        let current = state.lock().unwrap();
        (
            current.registered,
            current.boot.clone(),
            current.boot_accepted_generation,
            current.replay_requested,
        )
    };
    client.disconnect().await.unwrap();
    let native_calls = timeout(BOUND, peer).await.unwrap().unwrap();
    if replacement.is_some() {
        assert!(
            !registered,
            "late old Accepted completion must not register a denied replacement socket"
        );
        assert_eq!(remembered_boot.as_ref(), Some(&replacement_call.payload));
        assert_eq!(accepted_generation, None);
        assert!(!replay_requested);
        assert_eq!(model.snapshot()["offlineRecords"], 1);
        assert_eq!(model.snapshot()["uncertainRecords"], 0);
        assert!(
            native_calls.is_empty(),
            "offline native traffic must wait for current Accepted Boot"
        );
    } else {
        result.unwrap();
        assert!(registered);
        assert_eq!(accepted_generation, Some(original_generation));
        assert_eq!(model.snapshot()["offlineRecords"], 0);
        assert_eq!(native_calls, ["StartTransaction", "StopTransaction"]);
    }
}

async fn setup_client(
    endpoint: &str,
    state: &Arc<Mutex<Ocpp16State>>,
    traces: &TraceBuffer,
) -> (super::NativeClient, oneshot::Receiver<()>) {
    let (client, barrier, receiver) = crate::trigger_transport::connect(
        endpoint,
        None,
        Duration::from_secs(2),
        true,
        16,
        Arc::clone(state),
    )
    .await
    .unwrap();
    crate::trigger::register(
        &client,
        barrier,
        crate::trigger::TriggerSettings {
            connectors: vec![1],
            responses: crate::TriggerResponses::default(),
            observation: crate::TriggerObservation::default(),
        },
        Arc::clone(state),
        traces.clone(),
        receiver,
    )
    .await;
    let (reconnected, second_socket) = oneshot::channel();
    let reconnected = Arc::new(Mutex::new(Some(reconnected)));
    client
        .on_reconnect(move |_| {
            if let Some(sender) = reconnected.lock().unwrap().take() {
                sender.send(()).unwrap();
            }
            async {}
        })
        .await;
    (client, second_socket)
}

fn hold_completion(
    path: CompletionPath,
    adapter: NativeReplayClient,
    call: SimulatorCall,
    response: Value,
    held: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<(), crate::SimulatorClientError>> {
    tokio::spawn(async move {
        held.await.unwrap();
        match path {
            CompletionPath::Command => {
                let result = crate::client_runtime::finish_call(
                    &adapter.state,
                    &call,
                    Ok(response),
                    &adapter.traces,
                    crate::client_exchange16::NativeExchange::recovery(adapter.generation, false),
                );
                if result.is_ok() {
                    let mut replay = JoinSet::new();
                    crate::client_runtime::schedule_replay(
                        &mut replay,
                        &adapter.client,
                        &adapter.state,
                        &adapter.traces,
                    );
                    while let Some(joined) = replay.join_next().await {
                        joined.unwrap();
                    }
                }
                result.map(|_| ())
            }
            CompletionPath::Bootstrap => {
                adapter.finish_call(&call, &response)?;
                finish_bootstrap(&adapter, response).await
            }
            CompletionPath::FirstSend => {
                crate::client_exchange16::NativeExchange::recovery(adapter.generation, true)
                    .send(
                        &adapter.client,
                        &SimulatorCall {
                            action: SimulatorAction::StartTransaction,
                            payload: json!({"connectorId":1,"idTag":"GENERATION-PRIVATE",
                            "meterStart":600,"timestamp":"2026-10-04T00:00:01Z"}),
                        },
                    )
                    .await
                    .map(|_| ())
            }
        }
    })
}

async fn native_peer(
    listener: TcpListener,
    replacement: Option<&'static str>,
    close_allowed: oneshot::Receiver<()>,
) -> Vec<String> {
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    if let Some(status) = replacement {
        close_allowed.await.unwrap();
        socket.close(None).await.unwrap();
        socket = accept(&listener).await;
        let boot = receive(&mut socket).await;
        assert_eq!(boot[2], "BootNotification");
        assert_eq!(boot[3]["chargePointModel"], "Replacement Boot");
        send(
            &mut socket,
            json!([3,boot[1],{"status":status,
            "currentTime":"2026-10-04T00:00:02Z","interval":60}]),
        )
        .await;
    }
    let mut native_calls = Vec::new();
    loop {
        let call = receive(&mut socket).await;
        match call[2].as_str().unwrap() {
            "Heartbeat" => {
                send(
                    &mut socket,
                    json!([3,call[1],{"currentTime":"2026-10-04T00:00:03Z"}]),
                )
                .await;
                break;
            }
            "StartTransaction" => {
                native_calls.push("StartTransaction".to_owned());
                assert_eq!(call[3]["idTag"], "GENERATION-PRIVATE");
                assert_eq!(call[3]["meterStart"], 600);
                assert_eq!(call[3]["timestamp"], "2026-10-04T00:00:01Z");
                send(
                    &mut socket,
                    json!([3,call[1],{"transactionId":NATIVE_ID,
                    "idTagInfo":{"status":"Accepted"}}]),
                )
                .await;
            }
            "StopTransaction" => {
                native_calls.push("StopTransaction".to_owned());
                assert_eq!(call[3]["transactionId"], NATIVE_ID);
                assert_eq!(call[3]["meterStop"], 700);
                assert_eq!(call[3]["timestamp"], "2026-10-04T00:00:02Z");
                send(&mut socket, json!([3, call[1], {}])).await;
            }
            other => panic!("unexpected native action: {other}"),
        }
    }
    closed(&mut socket).await;
    native_calls
}
